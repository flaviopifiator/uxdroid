//! Streams on a real PulseAudio server, named by `UXDROID_TEST_PULSE` (for example
//! `unix:/tmp/pulse/native`); skipped without one. `runtime/framework/audio_smoke.sh` and
//! `benches/framework/audio.sh` start a private server with null devices and run these.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uxdroid_audio::{Backend, Config, Flow, Input, Output, State};

fn server() -> Option<Backend> {
    match std::env::var("UXDROID_TEST_PULSE") {
        Ok(server) => Some(Backend::Pulse(Some(server))),
        Err(_) => {
            eprintln!("skipped: UXDROID_TEST_PULSE names no server");
            None
        }
    }
}

#[test]
fn callbacks_come_once_per_period_at_the_servers_pace() {
    let Some(backend) = server() else { return };
    for rate in [48_000, 32_000] {
        let times = Arc::new(Mutex::new(Vec::new()));
        let seen = times.clone();
        let config = Config {
            rate,
            channels: 2,
            capacity: 0,
        };
        let output = Output::with_callback(&backend, config, move |out| {
            out.fill(0.1);
            let mut times = seen.lock().unwrap();
            times.push(Instant::now());
            if times.len() == 100 { Flow::Stop } else { Flow::Continue }
        })
        .unwrap();
        output.start();
        assert_eq!(output.wait_while(State::Playing, Duration::from_secs(5)), State::Stopped);
        let latency = output.latency();
        drop(output);
        let times = times.lock().unwrap();
        // The first periods fill the server's queue at once; then the server's clock paces them.
        let steady = &times[20..];
        let period = (steady[steady.len() - 1] - steady[0]) / (steady.len() as u32 - 1);
        println!("{rate} Hz: a callback every {period:?}, server latency {latency:?}");
        assert!(
            period > Duration::from_millis(8) && period < Duration::from_millis(12),
            "{rate} Hz: a callback every {period:?}"
        );
        assert!(latency < Duration::from_millis(60), "{rate} Hz: latency {latency:?}");
    }
}

#[test]
fn a_recording_arrives_at_the_servers_pace() {
    let Some(backend) = server() else { return };
    let config = Config {
        rate: 16_000,
        channels: 1,
        capacity: 1600,
    };
    let input = Input::open(&backend, config).unwrap();
    input.start();
    let started = Instant::now();
    let mut samples = vec![0.0; 8000];
    assert_eq!(input.read(&mut samples, true), 8000);
    let took = started.elapsed();
    // Half a second of sound takes half a second to record.
    assert!(took > Duration::from_millis(400) && took < Duration::from_millis(800), "{took:?}");
}
