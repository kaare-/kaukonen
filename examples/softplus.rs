use kaukonen::integrate::softplus;
use std::time::Instant;

fn main() {
    let n = 5_000_000;
    let xs: Vec<f32> = (0..1000).map(|i| (i as f32 / 100.0) - 5.0).collect();

    let start = Instant::now();
    let mut sink = 0.0f32;
    for i in 0..n {
        sink += softplus(xs[i % xs.len()]);
    }
    let sp = start.elapsed().as_secs_f64() * 1e9 / n as f64;

    let start = Instant::now();
    let mut sink2 = 0.0f32;
    for i in 0..n {
        let x = xs[i % xs.len()];
        sink2 += x * x - x * x * x;
    }
    let poly = start.elapsed().as_secs_f64() * 1e9 / n as f64;

    println!("softplus      {sp:6.2} ns  (sink {sink:.1})");
    println!("polynomial    {poly:6.2} ns  (sink {sink2:.1})");
    println!(
        "an RC-1 step calls softplus twice, so about {:.1} ns of its cost",
        2.0 * sp
    );
}
