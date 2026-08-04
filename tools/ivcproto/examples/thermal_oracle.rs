//! Host-side line protocol for differential testing of the native controller.

use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use ivcproto::neural::NeuralController;

fn parse_input(line: &str, line_number: usize) -> Result<[f32; 4]> {
    let values = line
        .split(',')
        .map(|value| {
            u32::from_str_radix(value, 16)
                .map(f32::from_bits)
                .with_context(|| format!("line {line_number}: invalid f32 bits `{value}`"))
        })
        .collect::<Result<Vec<_>>>()?;
    let Ok(inputs) = <[f32; 4]>::try_from(values) else {
        bail!("line {line_number}: expected four comma-separated f32 bit patterns");
    };
    Ok(inputs)
}

fn main() -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for (index, line) in stdin.lock().lines().enumerate() {
        let line_number = index + 1;
        let line = line.with_context(|| format!("read input line {line_number}"))?;
        let output = NeuralController
            .infer_normalized(parse_input(&line, line_number)?)
            .with_context(|| format!("evaluate input line {line_number}"))?;
        writeln!(stdout, "{:08x}", output.to_bits()).context("write oracle output")?;
    }
    stdout.flush().context("flush oracle output")?;
    Ok(())
}
