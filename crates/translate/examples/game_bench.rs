//! Runs the translation benchmark's game kernel from a library loaded through the bridge: the
//! native x86_64 build directly, the arm64 build through the translator. Each phase runs twice, so
//! the first run's extra time is translation. Prints one JSON object.
//!
//! Usage: game_bench <libgames.so>
#![cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]

use std::path::Path;
use std::time::Instant;

use uxdroid_bridge::elf::Machine;
use uxdroid_bridge::guest::host_function;
use uxdroid_bridge::{Library, load};

type Phase = extern "C" fn(u32) -> u64;
type Tick = extern "C" fn(u64) -> u64;

const PARTICLE_STEPS: u32 = 20_000;
const MATRIX_ROUNDS: u32 = 20_000;
const HOST_CALLS: u32 = 2_000_000;
const TICKS: u64 = 2_000_000;

fn function<F: Copy>(library: &Library, name: &str, signature: &str) -> F {
    let address = library.symbol(name).expect(name) as u64;
    let address = match library.machine() {
        Machine::X86_64 => address as usize,
        _ => host_function(address, signature).expect(name),
    };
    // SAFETY: F is the function pointer type of `signature`.
    unsafe { std::mem::transmute_copy(&address) }
}

/// Runs `phase` twice and returns both times in milliseconds and the second run's checksum.
fn twice(phase: impl Fn() -> u64) -> ([f64; 2], u64) {
    let mut times = [0.0; 2];
    let mut sum = 0;
    for time in &mut times {
        let start = Instant::now();
        sum = phase();
        *time = start.elapsed().as_secs_f64() * 1000.0;
    }
    (times, sum)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, library] = args.as_slice() else {
        eprintln!("usage: game_bench <libgames.so>");
        std::process::exit(2);
    };
    uxdroid_translate::install();
    let library = load(Path::new(library), &[]).unwrap_or_else(|error| panic!("{error}"));
    let particles: Phase = function(library, "game_particles", "u64(u32)");
    let matrices: Phase = function(library, "game_matrices", "u64(u32)");
    let host_calls: Phase = function(library, "game_host_calls", "u64(u32)");
    let tick: Tick = function(library, "game_tick", "u64(u64)");

    let phases = [
        ("particles", twice(|| particles(PARTICLE_STEPS))),
        ("matrices", twice(|| matrices(MATRIX_ROUNDS))),
        ("host_calls", twice(|| host_calls(HOST_CALLS))),
        ("tick", twice(|| (0..TICKS).fold(1, |state, _| tick(state)))),
    ];
    let fields: Vec<String> = phases
        .iter()
        .map(|(name, ([first, second], sum))| {
            format!("\"{name}\":{{\"ms\":[{first:.3},{second:.3}],\"sum\":\"{sum:016x}\"}}")
        })
        .collect();
    println!("{{{}}}", fields.join(","));
}
