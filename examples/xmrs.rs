use clap::Parser;
use xmrs::prelude::*;

#[derive(Parser)]
struct Cli {
    #[arg(short = 'f', long, required = true, value_name = "filename")]
    filename: Option<String>,

    /// Turn pattern informations on
    #[arg(short = 'd', long, default_value = "false")]
    debug: bool,
}

fn main() -> Result<(), std::io::Error> {
    let cli = Cli::parse();

    if let Some(filename) = cli.filename {
        if !cli.debug {
            println!("--===~ XmRs Module Info Example ~===--");
            println!("(c) 2024 Sébastien Béchet\n");
            println!("opening {}", filename);
        } else {
            print!("{}: ", filename.trim());
        }
        let contents = std::fs::read(filename.trim())?;
        match Module::load(&contents) {
            Ok(module) => {
                if !cli.debug {
                    println!("{:#?}", module);
                } else {
                    println!("OK");
                }
            }
            Err(e) => {
                println!("ERR: {:?} for {}", e, filename.trim());
            }
        }
    }
    Ok(())
}
