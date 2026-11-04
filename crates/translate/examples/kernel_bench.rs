//! Runs the translation benchmark's kernel from a library loaded through the bridge: the native
//! x86_64 build directly, the arm64 build through the translator. Each phase runs twice, so the
//! first run's extra time is translation. Prints one JSON object.
//!
//! Usage: kernel_bench <libkernel.so> <MiB>
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::path::Path;
use std::time::Instant;

use uxdroid_bridge::elf::Machine;
use uxdroid_bridge::guest::host_function;
use uxdroid_bridge::{Library, load};

type Fill = extern "C" fn(*mut u8, usize, u64);
type Sha = extern "C" fn(*const u8, usize, *mut u8);
type Deflate = extern "C" fn(*const u8, usize, *mut u8, usize) -> usize;

fn function<F: Copy>(library: &Library, name: &str, signature: &str) -> F {
    let address = library.symbol(name).expect(name) as u64;
    let address = match library.machine() {
        Machine::X86_64 => address as usize,
        _ => host_function(address, signature).expect(name),
    };
    // SAFETY: F is the function pointer type of `signature`.
    unsafe { std::mem::transmute_copy(&address) }
}

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn status_kib(file: &str, field: &str) -> u64 {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    text.lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, library, mib] = args.as_slice() else {
        eprintln!("usage: kernel_bench <libkernel.so> <MiB>");
        std::process::exit(2);
    };
    let size = mib.parse::<usize>().expect("MiB") << 20;

    uxdroid_translate::install();
    let start = Instant::now();
    let library = load(Path::new(library), &[]).unwrap_or_else(|error| panic!("{error}"));
    let load_ms = milliseconds(start);
    let fill: Fill = function(library, "kernel_fill", "void(ptr,u64,u64)");
    let sha: Sha = function(library, "kernel_sha256", "void(ptr,u64,ptr)");
    let deflate: Deflate = function(library, "kernel_deflate", "u64(ptr,u64,ptr,u64)");

    let mut input = vec![0u8; size];
    let mut output = vec![0u8; size + size / 1000 + (1 << 20)];
    let start = Instant::now();
    fill(input.as_mut_ptr(), size, 0x00C0_FFEE);
    let fill_ms = milliseconds(start);

    let mut digest = [0u8; 32];
    let mut sha_ms = Vec::new();
    let mut deflate_ms = Vec::new();
    let mut compressed = 0;
    for _ in 0..2 {
        let start = Instant::now();
        sha(input.as_ptr(), size, digest.as_mut_ptr());
        sha_ms.push(milliseconds(start));
        let start = Instant::now();
        compressed = deflate(input.as_ptr(), size, output.as_mut_ptr(), output.len());
        deflate_ms.push(milliseconds(start));
    }
    if std::env::var_os("KERNEL_BENCH_SMAPS").is_some() {
        print_largest_mappings();
    }
    let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    println!(
        "{{\"load_ms\":{load_ms:.3},\"fill_ms\":{fill_ms:.3},\"sha_ms\":[{:.3},{:.3}],\"deflate_ms\":[{:.3},{:.3}],\"digest\":\"{digest}\",\"compressed\":{compressed},\"peak_rss_kib\":{},\"private_dirty_kib\":{}}}",
        sha_ms[0],
        sha_ms[1],
        deflate_ms[0],
        deflate_ms[1],
        status_kib("/proc/self/status", "VmHWM:"),
        status_kib("/proc/self/smaps_rollup", "Private_Dirty:"),
    );
}

/// The mappings with the most private dirty memory, on stderr, to see where memory goes.
fn print_largest_mappings() {
    let text = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
    let mut mappings: Vec<(u64, String)> = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Private_Dirty:") {
            let kib = value.trim().trim_end_matches(" kB").parse().unwrap_or(0);
            mappings.push((kib, current.clone()));
        } else if line
            .split_whitespace()
            .next()
            .is_some_and(|word| word.contains('-'))
        {
            current = line.to_string();
        }
    }
    mappings.sort_by_key(|mapping| std::cmp::Reverse(mapping.0));
    for (kib, mapping) in mappings.iter().take(12) {
        eprintln!("{kib:>8} KiB  {mapping}");
    }
}
