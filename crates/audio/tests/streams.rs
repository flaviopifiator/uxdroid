//! Streams on the file backend: every sample a stream hands its sink is in a file, at the pace of a
//! real device clock.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uxdroid_audio::{Backend, Config, Flow, Input, Output, State};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!(
            "uxdroid-audio-{name}-{}-{:?}",
            std::process::id(),
            Instant::now()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(rate: u32, channels: u16) -> Config {
    Config {
        rate,
        channels,
        capacity: rate / 10,
    }
}

/// The one output file in `dir`, named after its rate and channels, as samples.
fn written(dir: &Path, rate: u32, channels: u16) -> Vec<f32> {
    let files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("output-"))
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(
        files[0].ends_with(&format!("-{rate}hz-{channels}ch.f32")),
        "{}",
        files[0]
    );
    std::fs::read(dir.join(&files[0]))
        .unwrap()
        .chunks(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn ramp(samples: usize) -> Vec<f32> {
    (0..samples).map(|i| i as f32 / samples as f32).collect()
}

#[test]
fn a_drained_stream_hands_the_sink_exactly_what_was_written() {
    let dir = Scratch::new("exact");
    let samples = ramp(2 * 1000);
    let output = Output::open(&Backend::File(dir.0.clone()), config(48_000, 2)).unwrap();
    assert_eq!(output.config().period(), 480);
    assert_eq!(output.write(&samples, true), samples.len());
    assert_eq!(output.state(), State::Stopped);
    assert!(output.start());
    output.stop();
    assert_eq!(
        output.wait_while(State::Draining, Duration::from_secs(2)),
        State::Stopped
    );
    assert_eq!(output.position(), 1000);
    assert_eq!(output.underruns(), 0, "draining is not an underrun");
    drop(output);
    let file = written(&dir.0, 48_000, 2);
    assert_eq!(&file[..samples.len()], &samples[..]);
    // The last period is completed with silence.
    assert_eq!(file.len() % (480 * 2), 0);
    assert!(file[samples.len()..].iter().all(|&s| s == 0.0));
}

#[test]
fn a_starved_stream_counts_one_underrun_per_gap_and_plays_silence() {
    let dir = Scratch::new("underrun");
    let output = Output::open(&Backend::File(dir.0.clone()), config(8_000, 1)).unwrap();
    let period = output.config().period() as usize;
    assert_eq!(period, 80);
    output.write(&vec![0.5; period], true);
    output.start();
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(output.underruns(), 1);
    assert_eq!(output.position(), period as u64);
    output.write(&vec![0.25; period], true);
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(output.underruns(), 2);
    output.pause();
    drop(output);
    let file = written(&dir.0, 8_000, 1);
    assert!(file[..period].iter().all(|&s| s == 0.5));
    let second = file.iter().position(|&s| s == 0.25).unwrap();
    assert!(second >= 2 * period, "silence plays during the gap");
    assert!(file[period..second].iter().all(|&s| s == 0.0));
}

#[test]
fn a_full_buffer_blocks_a_blocking_write_and_shortens_a_non_blocking_one() {
    let dir = Scratch::new("full");
    let output = Output::open(
        &Backend::File(dir.0.clone()),
        Config {
            rate: 8_000,
            channels: 1,
            capacity: 160,
        },
    )
    .unwrap();
    assert_eq!(output.write(&[0.1; 200], false), 160);
    assert_eq!(output.available(), 0);
    output.start();
    let started = Instant::now();
    // 80 more frames fit once one 10 ms period has played.
    assert_eq!(output.write(&[0.2; 80], true), 80);
    assert!(started.elapsed() < Duration::from_millis(500));
    output.pause();
    output.flush();
    assert_eq!(output.available(), 160);
}

#[test]
fn a_playback_rate_is_resampled_to_the_device_rate() {
    let dir = Scratch::new("rate");
    let output = Output::open(&Backend::File(dir.0.clone()), config(8_000, 1)).unwrap();
    assert!(output.set_rate(4_000));
    assert!(!output.set_rate(1));
    assert_eq!(output.rate(), 4_000);
    let input: Vec<f32> = (0..41).map(|i| i as f32 / 64.0).collect();
    output.write(&input, true);
    output.start();
    output.stop();
    output.wait_while(State::Draining, Duration::from_secs(2));
    drop(output);
    let file = written(&dir.0, 8_000, 1);
    // Half the source rate: every source frame, then the midpoint to the next.
    for (j, &sample) in file.iter().take(80).enumerate() {
        assert_eq!(sample, j as f32 / 128.0, "frame {j}");
    }
}

#[test]
fn volume_scales_each_channel() {
    let dir = Scratch::new("volume");
    let output = Output::open(&Backend::File(dir.0.clone()), config(8_000, 2)).unwrap();
    output.set_volume(0.5, 0.25);
    output.write(&[1.0, 1.0, -1.0, 0.5], true);
    output.start();
    output.stop();
    output.wait_while(State::Draining, Duration::from_secs(2));
    drop(output);
    assert_eq!(&written(&dir.0, 8_000, 2)[..4], [0.5, 0.25, -0.5, 0.125]);
}

#[test]
fn a_callback_fills_one_period_per_device_tick() {
    let dir = Scratch::new("callback");
    let times = Arc::new(Mutex::new(Vec::new()));
    let seen = times.clone();
    let mut counter = 0.0;
    let output = Output::with_callback(
        &Backend::File(dir.0.clone()),
        config(48_000, 1),
        move |out| {
            seen.lock().unwrap().push(Instant::now());
            for sample in out.iter_mut() {
                *sample = counter;
            }
            counter += 1.0;
            if counter == 20.0 {
                Flow::Stop
            } else {
                Flow::Continue
            }
        },
    )
    .unwrap();
    assert_eq!(
        output.write(&[0.0], true),
        0,
        "a callback stream takes no writes"
    );
    output.start();
    assert_eq!(
        output.wait_while(State::Playing, Duration::from_secs(2)),
        State::Stopped
    );
    assert_eq!(output.position(), 20 * 480);
    drop(output);
    let times = times.lock().unwrap();
    assert_eq!(times.len(), 20);
    let span = times[19] - times[0];
    // Nineteen 10 ms periods, paced by the device clock.
    assert!(
        span >= Duration::from_millis(170) && span < Duration::from_millis(260),
        "{span:?}"
    );
    let file = written(&dir.0, 48_000, 1);
    for (period, chunk) in file.chunks(480).enumerate() {
        assert!(chunk.iter().all(|&s| s == period as f32), "period {period}");
    }
}

#[test]
fn a_recording_reads_the_input_file_then_silence() {
    let dir = Scratch::new("record");
    let samples = ramp(300);
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    std::fs::write(dir.0.join("input.f32"), bytes).unwrap();
    let input = Input::open(&Backend::File(dir.0.clone()), config(16_000, 1)).unwrap();
    input.start();
    let mut read = vec![9.0; 400];
    assert_eq!(input.read(&mut read, true), 400);
    assert_eq!(&read[..300], &samples[..]);
    assert!(read[300..].iter().all(|&s| s == 0.0));
    assert!(input.position() >= 400);
    input.stop();
    assert_eq!(input.state(), State::Stopped);
}

#[test]
fn a_recording_callback_sees_each_period_and_a_full_buffer_counts_overruns() {
    let dir = Scratch::new("record-callback");
    std::fs::write(
        dir.0.join("input.f32"),
        [0x3f800000u32.to_le_bytes(); 200].concat(),
    )
    .unwrap();
    let got = Arc::new(Mutex::new(Vec::new()));
    let sink = got.clone();
    let input = Input::with_callback(
        &Backend::File(dir.0.clone()),
        config(8_000, 1),
        move |data| {
            sink.lock().unwrap().extend_from_slice(data);
            Flow::Continue
        },
    )
    .unwrap();
    input.start();
    std::thread::sleep(Duration::from_millis(60));
    input.stop();
    let got = got.lock().unwrap();
    assert!(got.len() >= 160 && got.len() % 80 == 0, "{}", got.len());
    assert!(got[..160].iter().all(|&s| s == 1.0));

    let small = Input::open(
        &Backend::File(dir.0.clone()),
        Config {
            rate: 8_000,
            channels: 1,
            capacity: 80,
        },
    )
    .unwrap();
    small.start();
    std::thread::sleep(Duration::from_millis(60));
    assert!(small.overruns() >= 1);
}

#[test]
fn configs_outside_android_limits_are_refused() {
    let dir = Scratch::new("limits");
    let backend = Backend::File(dir.0.clone());
    for bad in [
        config(3_999, 1),
        config(192_001, 1),
        config(48_000, 0),
        config(48_000, 9),
    ] {
        assert!(Output::open(&backend, bad).is_err(), "{bad:?}");
    }
    assert!(Output::open(&backend, config(4_000, 8)).is_ok());
}

#[test]
fn backends_parse_from_their_names() {
    assert_eq!(Backend::parse("null").unwrap(), Backend::Null);
    assert_eq!(
        Backend::parse("file:/x/y").unwrap(),
        Backend::File("/x/y".into())
    );
    assert_eq!(Backend::parse("pulse").unwrap(), Backend::Pulse(None));
    assert_eq!(
        Backend::parse("pulse:unix:/run/p").unwrap(),
        Backend::Pulse(Some("unix:/run/p".into()))
    );
    assert_eq!(
        Backend::parse("alsa").unwrap(),
        Backend::Alsa("default".into())
    );
    assert_eq!(
        Backend::parse("alsa:hw:0").unwrap(),
        Backend::Alsa("hw:0".into())
    );
    assert_eq!(Backend::parse("").unwrap(), Backend::Auto);
    let error = Backend::parse("oss").unwrap_err().to_string();
    assert!(error.contains("oss"), "{error}");
}

#[test]
fn the_null_backend_paces_and_discards() {
    let output = Output::open(&Backend::Null, config(8_000, 1)).unwrap();
    output.write(&[0.5; 160], true);
    output.start();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(output.position(), 160);
}

#[test]
fn an_unreachable_pulse_server_is_an_error_naming_it() {
    let backend = Backend::Pulse(Some("unix:/nonexistent/uxdroid-pulse".into()));
    let error = match Output::open(&backend, config(48_000, 2)) {
        Ok(_) => panic!("opened"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("pulse"), "{error}");
}

#[test]
fn alsa_plays_into_its_null_device() {
    let output = Output::open(&Backend::Alsa("null".into()), config(48_000, 2)).unwrap();
    output.write(&vec![0.5; 2 * 960], true);
    output.start();
    output.stop();
    assert_eq!(
        output.wait_while(State::Draining, Duration::from_secs(2)),
        State::Stopped
    );
    assert_eq!(output.position(), 960);
}

#[test]
fn a_write_or_read_with_a_timeout_returns_what_fitted_when_it_expires() {
    let dir = Scratch::new("timeout");
    let output = Output::open(
        &Backend::File(dir.0.clone()),
        Config {
            rate: 8_000,
            channels: 1,
            capacity: 80,
        },
    )
    .unwrap();
    let started = Instant::now();
    // Not playing, so the second period never fits.
    assert_eq!(
        output.write_within(&[0.5; 160], Some(Duration::from_millis(30))),
        80
    );
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(30) && waited < Duration::from_millis(500),
        "{waited:?}"
    );

    let input = Input::open(&Backend::File(dir.0.clone()), config(8_000, 1)).unwrap();
    let mut samples = [0.0; 800];
    let started = Instant::now();
    assert_eq!(
        input.read_within(&mut samples, Some(Duration::from_millis(30))),
        0,
        "not recording"
    );
    input.start();
    let read = input.read_within(&mut samples, Some(Duration::from_millis(35)));
    assert!((160..800).contains(&read), "{read}");
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn the_presented_frame_trails_the_position_by_the_device_latency() {
    let output = Output::open(&Backend::Null, config(8_000, 1)).unwrap();
    output.write(&[0.5; 400], true);
    output.start();
    std::thread::sleep(Duration::from_millis(30));
    let (position, presented) = (output.position(), output.presented());
    // A clock device has no latency of its own.
    assert_eq!(presented, position);
    assert!(position > 0);
}

#[test]
fn a_stream_dropped_inside_its_own_callback_ends_without_waiting_for_itself() {
    let holder: Arc<Mutex<Option<Output>>> = Arc::new(Mutex::new(None));
    let inside = holder.clone();
    let (done, finished) = std::sync::mpsc::channel();
    let output = Output::with_callback(&Backend::Null, config(8_000, 1), move |out| {
        out.fill(0.0);
        // The last reference goes away on the device thread.
        drop(inside.lock().unwrap().take());
        let _ = done.send(());
        Flow::Continue
    })
    .unwrap();
    output.start();
    *holder.lock().unwrap() = Some(output);
    finished.recv_timeout(Duration::from_secs(2)).unwrap();
}
