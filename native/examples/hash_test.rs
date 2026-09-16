
use std::path::Path;
use native::utils::demo_hasher::calculate_demo_key;

fn main() {
    let path_a = Path::new("./local/demos/ktps8w9-gorilla_gskill_rr2_h1.dem");
    let path_b = Path::new("./local/demos/ktps8w9-m00cat_gskill_rr2_h1.dem");

    let key_a = calculate_demo_key(path_a);
    let key_b = calculate_demo_key(path_b);

    println!("Player A (Gorilla): {:?}", key_a);
    println!("Player B (m00cat):  {:?}", key_b);
}