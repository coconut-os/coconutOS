// Cargo does not track linker scripts passed via rustflags; relink when it changes.
fn main() {
    println!("cargo::rerun-if-changed=../../targets/shard.ld");
}
