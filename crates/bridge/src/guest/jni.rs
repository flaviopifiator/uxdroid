//! JNI for guest libraries: guest `JNIEnv` and `JavaVM` objects whose function tables are guest
//! stubs calling the host's, and host wrappers for guest native methods, made from each method's
//! shorty.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use super::abi::{Class, guest_layout};
use super::jni_table::FUNCTIONS;
use super::{Cpu, Kind, Service, Target, add_service, callback, host_service};
use crate::loader::Library;
use crate::table::{Signature, Type};

/// A guest `JNIEnv`: guest code reads `functions`; `host` is the env it stands for.
#[repr(C)]
struct GuestObject {
    functions: u64,
    host: u64,
}

const INVOKE_FUNCTIONS: [(&str, Option<&str>); 8] = [
    ("reserved0", None),
    ("reserved1", None),
    ("reserved2", None),
    ("DestroyJavaVM", Some("i32(ptr)")),
    ("AttachCurrentThread", None),
    ("DetachCurrentThread", Some("i32(ptr)")),
    ("GetEnv", None),
    ("AttachCurrentThreadAsDaemon", None),
];

const JNI_OK: i32 = 0;
/// Above any real method count, so a damaged table cannot make the bridge allocate much.
const MAX_REGISTERED_METHODS: u64 = 1 << 16;
const MAX_SIGNATURE_BYTES: usize = 64 * 1024;

fn slot(name: &str) -> usize {
    FUNCTIONS
        .iter()
        .position(|(known, _)| *known == name)
        .unwrap_or(0)
}

fn leak_table(entries: Vec<u64>) -> u64 {
    Box::leak(entries.into_boxed_slice()).as_ptr() as u64
}

fn env_table() -> u64 {
    static TABLE: OnceLock<u64> = OnceLock::new();
    *TABLE.get_or_init(|| {
        let entries = FUNCTIONS
            .iter()
            .enumerate()
            .map(|(index, &(name, signature))| {
                let service = match (name, signature) {
                    (_, None) => return 0,
                    ("RegisterNatives", _) => add_service(Service::Special(register_natives)),
                    ("GetJavaVM", _) => add_service(Service::Special(get_java_vm)),
                    ("GetMethodID", _) => add_service(Service::Special(get_method_id)),
                    ("GetStaticMethodID", _) => add_service(Service::Special(get_static_method_id)),
                    (_, Some(signature))
                        if signature.ends_with(",...)") || signature.ends_with(",va_list)") =>
                    {
                        match MethodCall::of(name) {
                            Some(call) => add_service(Service::JniCall(call)),
                            None => add_service(Service::Missing(format!("JNIEnv::{name}"))),
                        }
                    }
                    (_, Some(signature)) => {
                        host_service(&format!("JNIEnv::{name}"), Target::JniEnv(index), signature)
                    }
                };
                service.unwrap_or(0)
            })
            .collect();
        leak_table(entries)
    })
}

fn vm_table() -> u64 {
    static TABLE: OnceLock<u64> = OnceLock::new();
    *TABLE.get_or_init(|| {
        let entries = INVOKE_FUNCTIONS
            .iter()
            .enumerate()
            .map(|(index, &(name, signature))| {
                let service = match (name, signature) {
                    ("GetEnv", _) => add_service(Service::Special(get_env)),
                    ("AttachCurrentThread", _) => add_service(Service::Special(attach)),
                    ("AttachCurrentThreadAsDaemon", _) => {
                        add_service(Service::Special(attach_daemon))
                    }
                    (_, None) => return 0,
                    (_, Some(signature)) => {
                        host_service(&format!("JavaVM::{name}"), Target::JavaVm(index), signature)
                    }
                };
                service.unwrap_or(0)
            })
            .collect();
        leak_table(entries)
    })
}

fn guest_object(objects: &Mutex<Option<HashMap<u64, u64>>>, host: u64, table: u64) -> u64 {
    let mut objects = objects
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *objects
        .get_or_insert_with(HashMap::new)
        .entry(host)
        .or_insert_with(|| {
            let object = Box::leak(Box::new(GuestObject {
                functions: table,
                host,
            }));
            std::ptr::from_mut(object) as u64
        })
}

/// The guest `JNIEnv` standing for a host one; null stays null.
pub(super) fn guest_env(host: u64) -> u64 {
    static ENVS: Mutex<Option<HashMap<u64, u64>>> = Mutex::new(None);
    thread_local! {
        static LAST: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
    }
    if host == 0 {
        return 0;
    }
    let (last_host, last_guest) = LAST.with(Cell::get);
    if last_host == host {
        return last_guest;
    }
    let guest = guest_object(&ENVS, host, env_table());
    LAST.with(|cell| cell.set((host, guest)));
    guest
}

pub(super) fn guest_vm(host: u64) -> u64 {
    static VMS: Mutex<Option<HashMap<u64, u64>>> = Mutex::new(None);
    if host == 0 {
        return 0;
    }
    guest_object(&VMS, host, vm_table())
}

fn host_of(guest: u64) -> u64 {
    if guest == 0 {
        return 0;
    }
    // SAFETY: guest envs and VMs are the leaked objects made above, handed to guest code, which
    // passes them back as each JNI function's first argument.
    unsafe { (*(guest as usize as *const GuestObject)).host }
}

pub(super) fn host_env(guest: u64) -> u64 {
    host_of(guest)
}

pub(super) fn host_vm(guest: u64) -> u64 {
    host_of(guest)
}

fn table_function(host: u64, slot: usize) -> u64 {
    if host == 0 {
        return 0;
    }
    // SAFETY: a host `JNIEnv*` or `JavaVM*` points at a pointer to its function table, which holds
    // every slot of the JNI version the tables list.
    unsafe {
        let table = *(host as usize as *const *const u64);
        *table.add(slot)
    }
}

pub(super) fn env_function(host_env: u64, slot: usize) -> u64 {
    table_function(host_env, slot)
}

pub(super) fn vm_function(host_vm: u64, slot: usize) -> u64 {
    table_function(host_vm, slot)
}

fn register_natives(cpu: &dyn Cpu) {
    type Register = unsafe extern "C" fn(u64, u64, *const [u64; 3], i32) -> i32;
    const JNI_ERR: i32 = -1;
    let host = host_env(cpu.x(0));
    let count = cpu.x(3) as i32;
    if count < 0 || count as u64 > MAX_REGISTERED_METHODS {
        cpu.set_x(0, JNI_ERR as i64 as u64);
        return;
    }
    let methods = cpu.x(2) as usize as *const [u64; 3];
    let mut wrapped = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        // SAFETY: the guest passes `count` JNINativeMethod records {name, signature, fnPtr},
        // which have the same layout on arm64 and the host.
        let [name, descriptor, function] = unsafe { *methods.add(index) };
        // SAFETY: the JNI contract makes the signature a C string; the read is bounded.
        let descriptor_text = unsafe { super::native::c_string(descriptor, MAX_SIGNATURE_BYTES) };
        let thunk = descriptor_shorty(descriptor_text)
            .and_then(|shorty| shorty_signature(&shorty))
            .ok_or_else(|| {
                format!(
                    "method signature {:?} cannot be carried",
                    String::from_utf8_lossy(descriptor_text)
                )
            })
            .and_then(|signature| callback(function, &signature, Kind::Jni));
        match thunk {
            Ok(thunk) => wrapped.push([name, descriptor, thunk as u64]),
            Err(reason) => super::fail(cpu, format_args!("RegisterNatives: {reason}")),
        }
    }
    let register = env_function(host, slot("RegisterNatives"));
    // SAFETY: the host's RegisterNatives, called with the host env, the guest's class reference
    // and records whose functions are host wrappers with each method's signature.
    let status = unsafe {
        std::mem::transmute::<usize, Register>(register as usize)(
            host,
            cpu.x(1),
            wrapped.as_ptr(),
            count,
        )
    };
    cpu.set_x(0, status as i64 as u64);
}

fn get_java_vm(cpu: &dyn Cpu) {
    type GetJavaVm = unsafe extern "C" fn(u64, *mut u64) -> i32;
    let host = host_env(cpu.x(0));
    let mut vm = 0u64;
    let function = env_function(host, slot("GetJavaVM"));
    // SAFETY: the host's GetJavaVM with the host env and a local result.
    let status =
        unsafe { std::mem::transmute::<usize, GetJavaVm>(function as usize)(host, &mut vm) };
    if status == JNI_OK {
        // SAFETY: the guest passes a writable `JavaVM**`.
        unsafe { super::native::write_u64(cpu.x(1), guest_vm(vm)) };
    }
    cpu.set_x(0, status as i64 as u64);
}

fn get_env(cpu: &dyn Cpu) {
    type GetEnv = unsafe extern "C" fn(u64, *mut u64, i32) -> i32;
    let host = host_vm(cpu.x(0));
    let mut env = 0u64;
    let function = vm_function(host, 6);
    // SAFETY: the host's GetEnv with the host VM, a local result and the guest's version.
    let status = unsafe {
        std::mem::transmute::<usize, GetEnv>(function as usize)(host, &mut env, cpu.x(2) as i32)
    };
    if status == JNI_OK {
        // SAFETY: the guest passes a writable `void**`.
        unsafe { super::native::write_u64(cpu.x(1), guest_env(env)) };
    }
    cpu.set_x(0, status as i64 as u64);
}

fn attach(cpu: &dyn Cpu) {
    attach_with(cpu, 4);
}

fn attach_daemon(cpu: &dyn Cpu) {
    attach_with(cpu, 7);
}

fn attach_with(cpu: &dyn Cpu, slot: usize) {
    type Attach = unsafe extern "C" fn(u64, *mut u64, u64) -> i32;
    let host = host_vm(cpu.x(0));
    let mut env = 0u64;
    let function = vm_function(host, slot);
    // SAFETY: the host's AttachCurrentThread; its optional arguments struct has the same
    // layout on both sides.
    let status = unsafe {
        std::mem::transmute::<usize, Attach>(function as usize)(host, &mut env, cpu.x(2))
    };
    if status == JNI_OK {
        // SAFETY: the guest passes a writable `JNIEnv**`.
        unsafe { super::native::write_u64(cpu.x(1), guest_env(env)) };
    }
    cpu.set_x(0, status as i64 as u64);
}

/// A `Call*Method` (variadic) or `Call*MethodV` (`va_list`), served by its `A` form, which takes
/// the method's arguments as a `jvalue` array the bridge builds.
pub(super) struct MethodCall {
    name: &'static str,
    a_slot: usize,
    /// The env, object or class, nonvirtual's class, and the method ID.
    fixed: usize,
    returns: Type,
    va_list: bool,
}

impl MethodCall {
    fn of(name: &'static str) -> Option<MethodCall> {
        let (base, va_list) = match name.strip_suffix('V') {
            Some(base) => (base, true),
            None => (name, false),
        };
        let a_name = format!("{base}A");
        let a_slot = FUNCTIONS.iter().position(|(known, _)| *known == a_name)?;
        let Ok(Signature::Function {
            returns,
            parameters,
            variadic: false,
        }) = Signature::parse(FUNCTIONS[a_slot].1?)
        else {
            return None;
        };
        Some(MethodCall {
            name,
            a_slot,
            fixed: parameters.len().checked_sub(1)?,
            returns,
            va_list,
        })
    }
}

/// The parameters of each method ID the guest got from `GetMethodID` or `GetStaticMethodID`, as
/// shorty letters: the descriptor is what tells where a variadic call's arguments are.
static METHOD_PARAMETERS: Mutex<Option<HashMap<u64, Box<[u8]>>>> = Mutex::new(None);

fn get_method_id(cpu: &dyn Cpu) {
    method_id(cpu, slot("GetMethodID"));
}

fn get_static_method_id(cpu: &dyn Cpu) {
    method_id(cpu, slot("GetStaticMethodID"));
}

fn method_id(cpu: &dyn Cpu, slot: usize) {
    type GetMethodId = unsafe extern "C" fn(u64, u64, u64, u64) -> u64;
    let host = host_env(cpu.x(0));
    let function = env_function(host, slot);
    if function == 0 {
        super::fail(
            cpu,
            format_args!("JNIEnv slot {slot} is null in the host table"),
        );
    }
    // SAFETY: the host's Get(Static)MethodID with the host env and the guest's class, name and
    // descriptor, which are host values as guest pointers are host pointers.
    let method = unsafe {
        std::mem::transmute::<usize, GetMethodId>(function as usize)(
            host,
            cpu.x(1),
            cpu.x(2),
            cpu.x(3),
        )
    };
    if method != 0 {
        // SAFETY: the JNI contract makes the descriptor a C string; the read is bounded.
        let descriptor = unsafe { super::native::c_string(cpu.x(3), MAX_SIGNATURE_BYTES) };
        if let Some(shorty) = descriptor_shorty(descriptor) {
            METHOD_PARAMETERS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get_or_insert_with(HashMap::new)
                .insert(method, shorty[1..].into());
        }
    }
    cpu.set_x(0, method);
}

/// A `jvalue` from a variadic argument as C promotes it: narrow integers arrive as `int` and a
/// `float` as `double`.
fn jvalue(letter: u8, raw: u64) -> u64 {
    match letter {
        b'Z' => u64::from(raw as u8),
        b'B' => raw as i8 as i64 as u64,
        b'C' => u64::from(raw as u16),
        b'S' => raw as i16 as i64 as u64,
        b'I' => raw as i32 as i64 as u64,
        b'F' => u64::from((f64::from_bits(raw) as f32).to_bits()),
        _ => raw,
    }
}

pub(super) fn call_method(cpu: &dyn Cpu, call: &MethodCall) {
    let method = cpu.x(call.fixed - 1);
    let parameters = METHOD_PARAMETERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .and_then(|known| known.get(&method).cloned());
    let Some(parameters) = parameters else {
        super::fail(
            cpu,
            format_args!(
                "JNIEnv::{} with a method ID not made by GetMethodID or GetStaticMethodID, whose descriptor the bridge cannot know",
                call.name
            ),
        );
    };
    let classes: Vec<Class> = parameters
        .iter()
        .map(|&letter| match letter {
            b'F' | b'D' => Class::Vector,
            _ => Class::Integer,
        })
        .collect();
    let raw = if call.va_list {
        super::va_arguments(cpu, call.name, cpu.x(call.fixed), &classes)
    } else {
        let mut all = vec![Class::Integer; call.fixed];
        all.extend(&classes);
        guest_layout(&all).slots[call.fixed..]
            .iter()
            .map(|&slot| super::read_slot(cpu, slot))
            .collect()
    };
    let arguments: Vec<u64> = parameters
        .iter()
        .zip(raw)
        .map(|(&letter, raw)| jvalue(letter, raw))
        .collect();

    let mut frame = super::native::HostFrame::default();
    frame.gpr[0] = host_env(cpu.x(0));
    for register in 1..call.fixed {
        frame.gpr[register] = cpu.x(register);
    }
    frame.gpr[call.fixed] = arguments.as_ptr() as u64;
    frame.function = env_function(frame.gpr[0], call.a_slot);
    if frame.function == 0 {
        super::fail(
            cpu,
            format_args!("JNIEnv::{} is null in the host table", call.name),
        );
    }
    // SAFETY: the host's `A` form takes the env, the guest's object or class references, the
    // method ID and a jvalue per parameter of the method's descriptor, all of which the frame holds.
    unsafe { super::native::host_call(&mut frame, &[]) };
    super::set_result(cpu, call.returns, frame.rax, frame.xmm0);
}

/// A method's shorty from its descriptor, `(IJ[Ljava/lang/String;)V` to `VIJL`.
pub(crate) fn descriptor_shorty(descriptor: &[u8]) -> Option<Vec<u8>> {
    let body = descriptor.strip_prefix(b"(")?;
    let close = body.iter().position(|&byte| byte == b')')?;
    let mut parameters = Vec::new();
    let mut rest = &body[..close];
    while !rest.is_empty() {
        let (kind, tail) = one_type(rest)?;
        parameters.push(kind);
        rest = tail;
    }
    let (returns, tail) = one_type(&body[close + 1..])?;
    if !tail.is_empty() {
        return None;
    }
    let mut shorty = vec![returns];
    shorty.extend(parameters);
    Some(shorty)
}

fn one_type(text: &[u8]) -> Option<(u8, &[u8])> {
    let (&first, mut rest) = text.split_first()?;
    match first {
        b'Z' | b'B' | b'C' | b'S' | b'I' | b'J' | b'F' | b'D' | b'V' => Some((first, rest)),
        b'L' => {
            let end = rest.iter().position(|&byte| byte == b';')?;
            Some((b'L', &rest[end + 1..]))
        }
        b'[' => {
            while let Some((b'[', tail)) = rest.split_first() {
                rest = tail;
            }
            let (element, tail) = one_type(rest)?;
            (element != b'V').then_some((b'L', tail))
        }
        _ => None,
    }
}

/// A native method's C signature from its shorty: the env and the class or object first.
pub(crate) fn shorty_signature(shorty: &[u8]) -> Option<String> {
    fn kind(letter: u8) -> Option<&'static str> {
        Some(match letter {
            b'Z' => "u8",
            b'B' => "i8",
            b'C' => "u16",
            b'S' => "i16",
            b'I' => "i32",
            b'J' => "i64",
            b'F' => "f32",
            b'D' => "f64",
            b'L' => "ptr",
            _ => return None,
        })
    }
    let (&returns, parameters) = shorty.split_first()?;
    let returns = if returns == b'V' {
        "void"
    } else {
        kind(returns)?
    };
    let mut signature = format!("{returns}(ptr,ptr");
    for &letter in parameters {
        signature.push(',');
        signature.push_str(kind(letter)?);
    }
    signature.push(')');
    Some(signature)
}

/// What ART calls for a guest library's symbol: a host wrapper of the guest function, made from
/// the method's shorty. `JNI_OnLoad` and `JNI_OnUnload` take the `JavaVM`.
pub fn trampoline(library: &Library, name: &str, shorty: Option<&[u8]>) -> Option<*mut c_void> {
    let address = library.symbol(name)? as u64;
    let made = match (name, shorty) {
        ("JNI_OnLoad" | "JNI_OnUnload", _) => callback(address, "i32(ptr,ptr)", Kind::OnLoad),
        (_, Some(shorty)) => shorty_signature(shorty)
            .ok_or_else(|| "bad shorty".to_string())
            .and_then(|signature| callback(address, &signature, Kind::Jni)),
        (_, None) => return None,
    };
    made.ok().map(|thunk| thunk as *mut c_void)
}

/// A host wrapper for a guest native method at `address`, from its shorty.
pub fn method_trampoline(address: u64, shorty: &[u8]) -> Option<*mut c_void> {
    let signature = shorty_signature(shorty)?;
    callback(address, &signature, Kind::Jni)
        .ok()
        .map(|thunk| thunk as *mut c_void)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_become_shorties() {
        assert_eq!(descriptor_shorty(b"(II)I").unwrap(), b"III");
        assert_eq!(descriptor_shorty(b"(IJDFIIIIID)D").unwrap(), b"DIJDFIIIIID");
        assert_eq!(
            descriptor_shorty(b"(Ljava/lang/String;[[I[Ljava/lang/Object;Z)V").unwrap(),
            b"VLLLZ"
        );
        for bad in [
            &b"II)I"[..],
            b"(II",
            b"(Ljava/lang)V",
            b"(I)",
            b"(I)VX",
            b"([V)V",
            b"(Q)V",
        ] {
            assert_eq!(
                descriptor_shorty(bad),
                None,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn shorties_become_signatures_with_env_and_class() {
        assert_eq!(shorty_signature(b"III").unwrap(), "i32(ptr,ptr,i32,i32)");
        assert_eq!(
            shorty_signature(b"VZLFD").unwrap(),
            "void(ptr,ptr,u8,ptr,f32,f64)"
        );
        assert_eq!(shorty_signature(b"V").unwrap(), "void(ptr,ptr)");
        assert_eq!(shorty_signature(b"IV"), None);
        assert_eq!(shorty_signature(b""), None);
    }

    #[test]
    fn variadic_and_va_list_calls_are_served_by_their_a_form() {
        let call = MethodCall::of("CallDoubleMethod").unwrap();
        assert_eq!(
            (call.a_slot, call.fixed, call.returns, call.va_list),
            (60, 3, Type::F64, false)
        );
        let call = MethodCall::of("CallNonvirtualIntMethodV").unwrap();
        assert_eq!(
            (call.a_slot, call.fixed, call.returns, call.va_list),
            (81, 4, Type::I32, true)
        );
        let call = MethodCall::of("NewObjectV").unwrap();
        assert_eq!((call.a_slot, call.fixed, call.returns), (30, 3, Type::Ptr));
        assert!(MethodCall::of("GetVersion").is_none());
    }

    #[test]
    fn promoted_variadic_arguments_become_jvalues() {
        assert_eq!(jvalue(b'Z', 0x1_01), 1);
        assert_eq!(jvalue(b'B', 0xFD), (-3i64) as u64);
        assert_eq!(jvalue(b'S', 0xFFFF_FFF9), (-7i64) as u64);
        assert_eq!(
            jvalue(b'F', 0.25f64.to_bits()),
            u64::from(0.25f32.to_bits())
        );
        assert_eq!(jvalue(b'D', 0.5f64.to_bits()), 0.5f64.to_bits());
    }

    #[test]
    fn the_jni_table_matches_the_jni_specification() {
        // Indices from the JNI specification's function table.
        assert_eq!(FUNCTIONS.len(), 233);
        assert_eq!(slot("GetVersion"), 4);
        assert_eq!(slot("FindClass"), 6);
        assert_eq!(slot("RegisterNatives"), 215);
        assert_eq!(slot("GetJavaVM"), 219);
        assert_eq!(slot("GetObjectRefType"), 232);
    }
}
