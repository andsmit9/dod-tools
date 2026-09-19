//! Throwaway diagnostic: print the first N frames of every IDEM section with
//! their command byte and time stamp, to check what playback clock the
//! engine will be gated on.
use std::env;

fn rd_i32(d: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}
fn rd_f32(d: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

fn main() {
    let path = env::args().nth(1).expect("usage: dump_times <file.dem> [n]");
    let n: usize = env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let data = std::fs::read(&path).unwrap();
    let dir_off = rd_i32(&data, 212) as usize;
    let count = rd_i32(&data, dir_off);
    const NAMES: [&str; 7] = ["?", "norewind", "read", "jumptime", "userdata", "usercmd", "stop"];

    for i in 0..count as usize {
        let p = dir_off + 4 + i * 88;
        let off = rd_i32(&data, p + 12) as usize;
        let len = rd_i32(&data, p + 16) as usize;
        println!("entry {i}: off={off} len={len}");
        let (mut q, end) = (off, off + len);
        let mut shown = 0;
        let mut last_t = f32::NAN;
        while q < end && shown < n {
            let cmd = data[q] as usize;
            let t = rd_f32(&data, q + 1);
            q += 5;
            println!("  cmd={:<9} t={:9.3}", NAMES.get(cmd).unwrap_or(&"?"), t);
            last_t = t;
            match cmd {
                1 | 2 => {
                    q += 28;
                    let mlen = rd_i32(&data, q) as usize;
                    q += 4 + mlen;
                }
                4 => {
                    let sz = rd_i32(&data, q);
                    q += 4 + sz.max(0) as usize;
                }
                5 => {
                    q += 8;
                    let nb = u16::from_le_bytes([data[q], data[q + 1]]) as usize;
                    q += 2 + nb;
                }
                6 => break,
                _ => {}
            }
            shown += 1;
        }
        // also find the last frame's time
        let _ = last_t;
    }
}
