use anyhow::Error;

use std::io::{stdin, stdout, Read, Write};

use sourcec::compile;

fn main() -> Result<(), Error> {
    let mut es_str = String::new();
    stdin().read_to_string(&mut es_str)?;

    let ll = compile(&es_str)?;

    stdout().write(ll.as_bytes())?;

    Ok(())
}
