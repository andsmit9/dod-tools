//! Adhoc diagnostic (not part of the fix, throwaway): walk the raw IDEM bytes
//! and print each dem_usercmd frame's outgoing_sequence/cmdnumber alongside
//! the incoming_sequence of the NetworkMessage frame that follows it, to
//! confirm the synthetic-frame sequence-backdating fix actually produces
//! distinct, increasing values instead of N identical ones.
use std::env;

fn rd_i32(data: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn main() {
    let path = env::args().nth(1).expect("usage: verify_usercmd_seq <file.dem>");
    let data = std::fs::read(&path).unwrap();

    let dir_off = rd_i32(&data, 212) as usize;
    let n = rd_i32(&data, dir_off);

    for i in 0..n as usize {
        let p = dir_off + 4 + i * 88; // ENTRY_SIZE
        let off = rd_i32(&data, p + 12) as usize;
        let len = rd_i32(&data, p + 16) as usize;
        println!("entry {i}: off={off} len={len}");

        let (mut q, end) = (off, off + len);
        let mut shown = 0;
        while q < end && shown < 16 {
            let cmd = data[q];
            q += 1;
            q += 4; // dt
            match cmd {
                1 | 2 => {
                    let seq = rd_i32(&data, q);
                    q += 28;
                    let mlen = rd_i32(&data, q) as usize;
                    q += 4;
                    println!("  [{shown}] {} incoming_sequence={seq}", if cmd == 1 { "dem_norewind" } else { "dem_read     " });
                    q += mlen;
                    shown += 1;
                }
                3 => {}
                4 => {
                    let sz = rd_i32(&data, q);
                    q += 4 + sz.max(0) as usize;
                }
                5 => {
                    let sequence = rd_i32(&data, q);
                    let cmdnumber = rd_i32(&data, q + 4);
                    println!("  [{shown}] dem_usercmd  outgoing_sequence={sequence} cmdnumber={cmdnumber}");
                    q += 8;
                    let nb = u16::from_le_bytes([data[q], data[q + 1]]) as usize;
                    q += 2 + nb;
                }
                6 => break,
                _ => {}
            }
        }
    }
}
