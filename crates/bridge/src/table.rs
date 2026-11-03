//! Signature tables: the functions and objects uxdroid exposes to app native code.
//!
//! Each Android system library an app links against, such as `libandroid.so` or `libEGL.so`, is a
//! [`Table`] declared next to its host implementation. A library that names one in `DT_NEEDED` binds
//! to the table's addresses, at no added cost. The signatures are what a translator for another ABI
//! needs to generate guest-to-host trampolines, so they use a closed vocabulary instead of C text:
//!
//! - a function is `ret(arg,arg)`, with `...` as the last argument when variadic;
//! - an object is its type alone;
//! - types are `void`, `i8`, `u8`, `i16`, `u16`, `i32`, `u32`, `i64`, `u64`, `f32`, `f64`, `ptr`
//!   (any pointer, including to functions) and `va_list` (a pointer to the System V register save
//!   header, the same in bionic and glibc on x86_64).

use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::fmt;
use std::sync::Mutex;

/// Where an export's code or data is.
#[derive(Clone, Copy)]
pub enum Address {
    /// A host function with the export's signature.
    Function(*const ()),
    /// A host object with the export's type, living as long as the process.
    Data(*const ()),
    /// A function another host library defines, such as Mesa's GLES; looked up when bound.
    Resolved(fn(&CStr) -> Option<*const c_void>),
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Address::Function(address) => write!(f, "Function({address:p})"),
            Address::Data(address) => write!(f, "Data({address:p})"),
            Address::Resolved(_) => write!(f, "Resolved"),
        }
    }
}

/// One function or object of a [`Table`].
#[derive(Clone, Copy, Debug)]
pub struct Export {
    pub name: &'static CStr,
    pub signature: &'static str,
    pub address: Address,
}

// SAFETY: the addresses point at immutable code or at objects that outlive the process' threads.
unsafe impl Send for Export {}
// SAFETY: as above.
unsafe impl Sync for Export {}

impl Export {
    pub const fn function(
        name: &'static CStr,
        signature: &'static str,
        address: *const (),
    ) -> Self {
        Export {
            name,
            signature,
            address: Address::Function(address),
        }
    }

    pub const fn data(name: &'static CStr, signature: &'static str, address: *const ()) -> Self {
        Export {
            name,
            signature,
            address: Address::Data(address),
        }
    }

    pub const fn resolved(
        name: &'static CStr,
        signature: &'static str,
        resolver: fn(&CStr) -> Option<*const c_void>,
    ) -> Self {
        Export {
            name,
            signature,
            address: Address::Resolved(resolver),
        }
    }

    pub fn is_data(&self) -> bool {
        matches!(self.address, Address::Data(_))
    }

    /// The host address to bind, or `None` when the host library lacks it.
    pub fn bind(&self) -> Option<usize> {
        match self.address {
            Address::Function(address) | Address::Data(address) => Some(address as usize),
            Address::Resolved(resolver) => resolver(self.name)
                .filter(|address| !address.is_null())
                .map(|address| address as usize),
        }
    }
}

/// An Android system library served from the host: its `DT_NEEDED` name and exports.
#[derive(Debug)]
pub struct Table {
    pub library: &'static str,
    pub exports: &'static [Export],
}

impl Table {
    pub fn find(&'static self, name: &[u8]) -> Option<&'static Export> {
        index(self).get(name).copied()
    }
}

/// One argument or return type of a signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Void,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    Ptr,
    VaList,
}

impl Type {
    fn parse(token: &str) -> Option<Type> {
        Some(match token {
            "void" => Type::Void,
            "i8" => Type::I8,
            "u8" => Type::U8,
            "i16" => Type::I16,
            "u16" => Type::U16,
            "i32" => Type::I32,
            "u32" => Type::U32,
            "i64" => Type::I64,
            "u64" => Type::U64,
            "f32" => Type::F32,
            "f64" => Type::F64,
            "ptr" => Type::Ptr,
            "va_list" => Type::VaList,
            _ => return None,
        })
    }
}

/// A parsed export signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signature {
    Function {
        returns: Type,
        parameters: Vec<Type>,
        variadic: bool,
    },
    Object(Type),
}

impl Signature {
    pub fn parse(text: &str) -> Result<Signature, String> {
        let invalid = || format!("invalid signature {text:?}");
        let Some(open) = text.find('(') else {
            return match Type::parse(text) {
                Some(Type::Void | Type::VaList) | None => Err(invalid()),
                Some(kind) => Ok(Signature::Object(kind)),
            };
        };
        let body = text[open + 1..].strip_suffix(')').ok_or_else(invalid)?;
        let returns = Type::parse(&text[..open]).ok_or_else(invalid)?;
        if returns == Type::VaList {
            return Err(invalid());
        }
        let mut parameters = Vec::new();
        let mut variadic = false;
        if !body.is_empty() {
            let tokens: Vec<&str> = body.split(',').collect();
            for (position, token) in tokens.iter().enumerate() {
                if *token == "..." && position + 1 == tokens.len() && position > 0 {
                    variadic = true;
                    continue;
                }
                match Type::parse(token) {
                    Some(Type::Void) | None => return Err(invalid()),
                    Some(kind) => parameters.push(kind),
                }
            }
        }
        Ok(Signature::Function {
            returns,
            parameters,
            variadic,
        })
    }
}

struct Registry {
    tables: Vec<&'static Table>,
    indices: HashMap<usize, &'static HashMap<&'static [u8], &'static Export>>,
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<T>(action: impl FnOnce(&mut Registry) -> T) -> T {
    let mut guard = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
    let registry = guard.get_or_insert_with(|| Registry {
        tables: vec![&crate::liblog::TABLE],
        indices: HashMap::new(),
    });
    action(registry)
}

fn index(table: &'static Table) -> &'static HashMap<&'static [u8], &'static Export> {
    with_registry(|registry| {
        let index: &mut &'static HashMap<_, _> = registry
            .indices
            .entry(std::ptr::from_ref(table) as usize)
            .or_insert_with(move || {
                let map = table
                    .exports
                    .iter()
                    .map(|export| (export.name.to_bytes(), export))
                    .collect();
                Box::leak(Box::new(map))
            });
        *index
    })
}

/// Serves `table` to every library loaded from now on that needs its library. The first table for a
/// name wins, so the host's own choice cannot be replaced by a later caller.
pub fn provide(table: &'static Table) {
    with_registry(|registry| {
        if !registry.tables.iter().any(|t| t.library == table.library) {
            registry.tables.push(table);
        }
    });
}

/// The table serving `library`, if any.
pub fn find(library: &str) -> Option<&'static Table> {
    with_registry(|registry| {
        registry
            .tables
            .iter()
            .find(|table| table.library == library)
            .copied()
    })
}

/// `liblog.so`'s table, which every library sees.
pub(crate) fn liblog() -> &'static Table {
    &crate::liblog::TABLE
}

/// Every table, for lookups in no particular library such as `dlsym(RTLD_DEFAULT)`.
pub fn all() -> Vec<&'static Table> {
    with_registry(|registry| registry.tables.clone())
}

/// Whether `handle` is a table's handle from `dlopen`.
pub(crate) fn from_handle(handle: *const c_void) -> Option<&'static Table> {
    with_registry(|registry| {
        registry
            .tables
            .iter()
            .find(|table| std::ptr::from_ref(**table).cast::<c_void>() == handle)
            .copied()
    })
}

/// A name and signature check for a whole table, run by each table's own tests.
pub fn validate(table: &Table) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for export in table.exports {
        let name = export.name.to_str().map_err(|_| "non-UTF-8 name")?;
        if !seen.insert(name) {
            return Err(format!("{}: {name} is declared twice", table.library));
        }
        let signature = Signature::parse(export.signature)
            .map_err(|error| format!("{}: {name}: {error}", table.library))?;
        if export.is_data() != matches!(signature, Signature::Object(_)) {
            return Err(format!(
                "{}: {name}: an object needs a type and a function a signature",
                table.library
            ));
        }
    }
    Ok(())
}
