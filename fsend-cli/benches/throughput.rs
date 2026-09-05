//! Throughput of a cli transfer over both iroh and WebRTC.
//!
//!     cargo bench
//!     FSEND_BENCH_MIB=1024 FSEND_BENCH_RUNS=5 cargo bench
//!     cargo bench -- webrtc

use std::future::Future;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use fsend_cli::iroh::IrohTransfer;
use fsend_cli::relay::ConnectionInfo;
use fsend_cli::transfer::{ReceiveArgs, SendArgs, Transfer};
use fsend_cli::webrtc::{WebRtcConfig, WebRtcTransfer};
use iroh::endpoint::presets;
use iroh::Endpoint;

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Incompressible content.
fn write_payload(path: &Path, bytes: u64) {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut buf = vec![0u8; 1 << 16];
    let mut left = bytes;
    while left > 0 {
        for chunk in buf.chunks_mut(8) {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            chunk.copy_from_slice(
                &state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes()[..chunk.len()],
            );
        }
        let n = left.min(buf.len() as u64) as usize;
        file.write_all(&buf[..n]).unwrap();
        left -= n as u64;
    }
    file.flush().unwrap();
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn send_harness(payload: PathBuf, transfer: impl Future<Output = Box<dyn Transfer>>) {
    runtime().block_on(async {
        let mut transfer = transfer.await;
        transfer
            .send_files(
                SendArgs {
                    files: vec![payload],
                },
                &mut |_| {},
                &mut || {},
                &mut |_| {},
            )
            .await
            .expect("send");
    });
}

fn receive_harness(
    out: PathBuf,
    bytes: u64,
    transfer: impl Future<Output = Box<dyn Transfer>>,
) -> f64 {
    runtime().block_on(async {
        let mut transfer = transfer.await;
        let mut started = None;
        let out_dir = out.clone();
        transfer
            .receive_files(
                ReceiveArgs { resume: false },
                &mut |_| started = Some(Instant::now()),
                &mut |_| Some(out_dir.clone()),
                &mut |_| {},
            )
            .await
            .expect("receive");
        let secs = started.expect("offer accepted").elapsed().as_secs_f64();
        bytes as f64 / secs / (1024.0 * 1024.0)
    })
}

fn loopback_info(endpoint: &Endpoint) -> ConnectionInfo {
    let addrs = endpoint
        .bound_sockets()
        .into_iter()
        .map(|bound| {
            let ip: IpAddr = if bound.is_ipv4() {
                Ipv4Addr::LOCALHOST.into()
            } else {
                Ipv6Addr::LOCALHOST.into()
            };
            SocketAddr::new(ip, bound.port()).to_string()
        })
        .collect();
    ConnectionInfo::Iroh {
        node_id: endpoint.id().to_string(),
        addrs,
    }
}

fn bench_iroh(payload: &Path, out: &Path, bytes: u64) -> f64 {
    let (addr_tx, addr_rx) = mpsc::channel();

    let sender = thread::spawn({
        let payload = payload.to_path_buf();
        move || {
            send_harness(payload, async {
                let endpoint = IrohTransfer::bind(presets::Minimal).await.unwrap();
                addr_tx.send(loopback_info(&endpoint)).unwrap();
                Box::new(IrohTransfer::accept(endpoint).await.unwrap()) as Box<dyn Transfer>
            })
        }
    });
    let receiver = thread::spawn({
        let out = out.to_path_buf();
        move || {
            receive_harness(out, bytes, async {
                let endpoint = IrohTransfer::bind(presets::Minimal).await.unwrap();
                let peer = addr_rx.recv().unwrap();
                Box::new(IrohTransfer::connect(endpoint, peer).await.unwrap()) as Box<dyn Transfer>
            })
        }
    });

    let rate = receiver.join().unwrap();
    sender.join().unwrap();
    rate
}

fn bench_webrtc(payload: &Path, out: &Path, bytes: u64) -> f64 {
    let config = WebRtcConfig {
        stun_servers: vec![],
        bind_addrs: vec!["127.0.0.1:0".into()],
        loopback: true,
        ice_timeouts: None,
    };
    let (offer_tx, offer_rx) = mpsc::channel::<String>();
    let (answer_tx, answer_rx) = mpsc::channel::<String>();

    let sender = thread::spawn({
        let (payload, config) = (payload.to_path_buf(), config.clone());
        move || {
            send_harness(payload, async {
                let (mut offerer, offer) =
                    WebRtcTransfer::create_offerer_with(&config).await.unwrap();
                offer_tx.send(offer).unwrap();
                offerer
                    .set_answer(&answer_rx.recv().unwrap())
                    .await
                    .unwrap();
                offerer.wait_connected().await.unwrap();
                Box::new(offerer) as Box<dyn Transfer>
            })
        }
    });
    let receiver = thread::spawn({
        let out = out.to_path_buf();
        move || {
            receive_harness(out, bytes, async {
                let offer = offer_rx.recv().unwrap();
                let (mut answerer, answer) = WebRtcTransfer::create_answerer_with(&offer, &config)
                    .await
                    .unwrap();
                answer_tx.send(answer).unwrap();
                answerer.wait_connected().await.unwrap();
                Box::new(answerer) as Box<dyn Transfer>
            })
        }
    });

    let rate = receiver.join().unwrap();
    sender.join().unwrap();
    rate
}

fn main() {
    let filter: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let runs = env_or("FSEND_BENCH_RUNS", 3);
    let bytes = env_or("FSEND_BENCH_MIB", 512) * 1024 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload.bin");
    write_payload(&payload, bytes);
    println!(
        "fsend throughput: {} MiB per transfer, {runs} runs per transport, both peers local\n",
        bytes / 1024 / 1024
    );

    type Bench = fn(&Path, &Path, u64) -> f64;
    let benches: [(&str, Bench); 2] = [("iroh", bench_iroh), ("webrtc", bench_webrtc)];
    for (name, bench) in benches {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let mut rates = Vec::new();
        for run in 1..=runs {
            let out = dir.path().join(format!("out-{name}-{run}"));
            let rate = bench(&payload, &out, bytes);
            let received = std::fs::metadata(out.join("payload.bin"))
                .map(|m| m.len())
                .unwrap_or(0);
            assert_eq!(received, bytes, "{name} run {run}: incomplete transfer");
            let _ = std::fs::remove_dir_all(&out);
            println!("{name:>7} run {run}: {rate:8.1} MiB/s");
            rates.push(rate);
        }
        rates.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "{name:>7}: median {:8.1} MiB/s  (min {:.1}, max {:.1})\n",
            rates[rates.len() / 2],
            rates[0],
            rates[rates.len() - 1]
        );
    }
}
