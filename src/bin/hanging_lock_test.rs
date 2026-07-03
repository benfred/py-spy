// Helper program for the hanging lock test.
//
// Waits until it receives input on stdin before exiting.
use std::io::{self, Write};

fn main() -> io::Result<()> {
    println!("awaiting input");
    io::stdout().flush()?;

    let mut buffer = String::new();
    let stdin = io::stdin(); // We get `Stdin` here.
    stdin.read_line(&mut buffer)?;
    println!("Read buffer: {buffer}. Exiting...");
    io::stdout().flush()?;
    Ok(())
}
