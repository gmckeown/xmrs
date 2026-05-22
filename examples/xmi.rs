#![forbid(unsafe_code)]

use xmrs::import::bin_reader::ImportError;
use xmrs::import::xm::xi_instrument::XiInstrument;

const XI: &[u8] = include_bytes!("instr.xi");

fn main() -> Result<(), ImportError> {
    let xmi = XiInstrument::load(XI)?;
    println!("Load XMI: {:#x?}", xmi);
    let instr = xmi.to_instrument();
    println!("Convert to instrument: {:#x?}", instr);

    Ok(())
}
