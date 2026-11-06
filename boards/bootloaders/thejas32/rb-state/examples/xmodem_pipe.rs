//! Host interop check: runs the XMODEM receiver over stdin/stdout so a real
//! sender (Python's `xmodem`, as used by tools/update.py) can talk to it.
//! Writes what it received to the file named by argv[1].
use rb_state::xmodem::{receive, Channel};
use std::io::{Read, Write};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

struct Pipe {
    rx: Receiver<u8>,
    out: std::io::Stdout,
}
impl Channel for Pipe {
    fn read_byte(&mut self, timeout_ms: u32) -> Option<u8> {
        self.rx.recv_timeout(Duration::from_millis(timeout_ms as u64)).ok()
    }
    fn write_byte(&mut self, b: u8) {
        self.out.write_all(&[b]).unwrap();
        self.out.flush().unwrap();
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("output file");
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for b in std::io::stdin().bytes() {
            if tx.send(b.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut pipe = Pipe { rx, out: std::io::stdout() };
    let mut buf = vec![0u8; 0x2_8000];
    match receive(&mut pipe, &mut buf) {
        Ok(n) => {
            std::fs::write(&path, &buf[..n]).unwrap();
            eprintln!("received {n} bytes");
        }
        Err(e) => {
            eprintln!("receive failed: {e:?}");
            std::process::exit(1);
        }
    }
}
