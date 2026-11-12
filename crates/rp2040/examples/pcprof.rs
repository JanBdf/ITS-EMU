//! PC histogram of both Pico cores: `pcprof BOOTROM UF2 [SECONDS]`.
//! `NOIDLE=1` disables idle skipping, `TRACE=FILE` logs every sample.

use std::collections::HashMap;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let rom = std::fs::read(&a[1]).unwrap();
    let uf2 = std::fs::read(&a[2]).unwrap();
    let secs: f64 = a.get(3).map_or(1.0, |s| s.parse().unwrap());
    let mut pico = rp2040::Pico::new(&rom, &uf2).unwrap();
    pico.set_idle_skipping(std::env::var_os("NOIDLE").is_none());
    let mut trace = std::env::var_os("TRACE").map(|p| std::io::BufWriter::new(std::fs::File::create(p).unwrap()));
    let mut h: [HashMap<u32, u32>; 2] = [HashMap::new(), HashMap::new()];
    let t0 = std::time::Instant::now();
    let end = (secs * 1e12) as u64;
    let step = 7_000_000; // 7 us
    let mut t = pico.time_ps();
    while t < end {
        t += step;
        pico.run_until_ps(t).unwrap();
        for (n, h) in h.iter_mut().enumerate() {
            *h.entry(pico.pc(n)).or_default() += 1;
            if let Some(w) = trace.as_mut() {
                use std::io::Write;
                writeln!(w, "{t} {n} {:08x} {}", pico.pc(n), pico.core(n).clock().cycles()).unwrap();
            }
        }
    }
    let wall = t0.elapsed().as_secs_f64();
    println!("virtual {secs} s wall {wall:.3} s ({:.2}x)", secs / wall);
    for (n, h) in h.iter().enumerate() {
        let mut v: Vec<_> = h.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        println!("core {n}:");
        for (pc, c) in v.iter().take(15) {
            println!("  {pc:08x} {c}");
        }
    }
}
