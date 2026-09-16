use dem::open_demo_from_bytes;
use dem::types::{FrameData, MessageData, NetMessage};
use dod::{UserMessage, Weapon};
use std::fs;

fn main() {
    let path = "local/demos/FPSDROPPINGMATCHvsIcyaxis.dem";
    println!("Scanning ALL Garand CurWeapon in: {}", path);
    let file_bytes = fs::read(path).unwrap();
    let demo = open_demo_from_bytes(&file_bytes).unwrap();

    let mut count = 0;
    for entry in &demo.directory.entries {
        for frame in &entry.frames {
            if let FrameData::NetworkMessage(box_type) = &frame.frame_data
                && let MessageData::Parsed(msgs) = &box_type.1.messages {
                    for net_msg in msgs {
                        if let NetMessage::UserMessage(user_msg) = net_msg {
                            let mut name_len = user_msg.name.len();
                            while name_len > 0 && user_msg.name[name_len - 1] == 0 {
                                name_len -= 1;
                            }
                            if &user_msg.name[..name_len] == b"CurWeapon"
                                && let Ok(UserMessage::CurWeapon(msg)) =
                                    UserMessage::new(&user_msg.name, &user_msg.data)
                                    && (msg.weapon == Weapon::Garand
                                        || msg.weapon == Weapon::ButtStock)
                                    {
                                        println!(
                                            "Frame time: {:.3} | CurWeapon: active={}, weapon={:?}, clip_ammo={}",
                                            frame.time, msg.is_active, msg.weapon, msg.clip_ammo
                                        );
                                        count += 1;
                                    }
                        }
                    }
                }
        }
    }
    println!("Total Garand CurWeapon: {}", count);
}
