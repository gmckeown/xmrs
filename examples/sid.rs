#![forbid(unsafe_code)]

use xmrs::tracker::import::bin_reader::ImportError;
use xmrs::tracker::import::sid::sid_module::SidModule;

fn main() -> Result<(), ImportError> {
    println!("--===~ XmRs SID Module Info Example ~===--");
    println!("(c) 2024 Sébastien Béchet\n");

    // Smoke-test every bundled SID parser: each call should complete
    // without panicking. The returned values are intentionally discarded
    // (prefixed `_` so no unused-variable warnings). `get_sid_delta`
    // uses the v30 phrase decoder which doesn't model phrase-level
    // compression or per-channel pattern loops; the defensive row cap
    // on `Voices::get_voice_grid` keeps it bounded.
    let _ = SidModule::get_sid_commando();
    let _ = SidModule::get_sid_crazy_comets();
    let _ = SidModule::get_sid_monty_on_the_run();
    let _ = SidModule::get_sid_last_v8();
    let _ = SidModule::get_sid_thing_on_a_spring();
    let _ = SidModule::get_sid_zoid();
    let _ = SidModule::get_sid_ace_2();
    let _ = SidModule::get_sid_delta();
    let _ = SidModule::get_sid_human_race();
    let _ = SidModule::get_sid_international_karate();
    let _ = SidModule::get_sid_lightforce();
    let _ = SidModule::get_sid_sanxion_song_1();
    let _ = SidModule::get_sid_sanxion_song_2();
    let _ = SidModule::get_sid_spellbound();
    let _ = SidModule::get_sid_thrust();

    Ok(())
}
