//! IT file header (192 bytes on disk).
//!
//! ```text
//! [u8;4]   id ("IMPM")
//! [u8;26]  song_name
//! u8       rows_per_beat
//! u8       rows_per_measure
//! u16      order_number
//! u16      instrument_number
//! u16      sample_number
//! u16      pattern_number
//! u16      created_with_tracker
//! u16      compatible_with_tracker
//! u16      flags
//! u16      special_flags
//! u8       global_volume
//! u8       mix_volume
//! u8       initial_speed
//! u8       initial_bpm
//! u8       pan_separation
//! u8       pitch_wheel_depth
//! u16      message_length
//! u32      message_offset
//! [u8;4]   reserved ("OMPT" for OpenMPT files)
//! [u8;64]  initial_channel_pan
//! [u8;64]  initial_channel_volume
//! ```

use alloc::string::String;

use crate::import::bin_reader::{bytes_to_trimmed_string, BinReader, ImportError};

/// On-disk size of [`ItHeader`].
pub const IT_HEADER_SIZE: usize = 192;

/// IT file header.
#[derive(Debug)]
pub struct ItHeader {
    /// Identifier ("IMPM").
    id: String,

    /// Song name
    pub song_name: String,

    /// Pattern information: Represents the number of rows per beat.
    pub rows_per_beat: u8,

    /// Pattern information: Represents the number of rows per measure.
    pub rows_per_measure: u8,

    /// Number of sequenced patterns in the song.
    pub order_number: u16,

    /// Number of instruments in the song.
    pub instrument_number: u16,

    /// Number of samples in the song.
    pub sample_number: u16,

    /// Number of patterns in the song.
    pub pattern_number: u16,

    /// ID of the tracker that made this file (1.0 to 2.17, encoded as hexadecimal).
    pub created_with_tracker: u16,

    /// Version of the tracker compatible with this file (1.0 to 2.17, encoded as hexadecimal).
    pub compatible_with_tracker: u16,

    /// Configuration flags (16 bits).
    /// - Bit 0: Stereo if on, mono if off.
    /// - Bit 1: Mixing (obsolete since version 1.04).
    /// - Bit 2: Use instruments if on, use samples if off.
    /// - Bit 3: Use linear slides if on, use Amiga slides if off.
    /// - Bit 4: Use old effects if on, use IT effects if off.
    /// - Bit 5: Link G effect with E and F memory if on.
    /// - Bit 6: MIDI pitch controlled if on.
    /// - Bit 7: Request Embedded MIDI Macros if on.
    /// - Bits 8-15 are reserved.
    pub flags: u16,

    /// Special configuration flags (16 bits).
    /// - Bit 0: Song message attached if on.
    /// - Bit 1: Edit history embedded if on (Some versions of Schism tracker set this off even if there is embedded history).
    /// - Bit 2: Highlight embedded if on.
    /// - Bit 3: Embedded MIDI Macro.
    /// - Bits 4-15 are reserved.
    pub special_flags: u16,

    /// Global volume of the song (0-128).
    pub global_volume: u8,

    /// Mixing volume of the song (0-128).
    pub mix_volume: u8,

    /// Starting tick speed of the song.
    pub initial_speed: u8,

    /// Starting beats per minute of the song.
    pub initial_bpm: u8,

    /// Panning separation between channels (0-128).
    pub pan_separation: u8,

    /// Pitch wheel depth for MIDI controllers.
    pub pitch_wheel_depth: u8,

    /// Length of the attached message.
    pub message_length: u16,

    /// Offset of the message in the file.
    pub message_offset: u32,

    /// Reserved field ("OMPT" for interpreted Mod Plug files).
    pub reserved: String,

    /// Initial pan of the channels
    /// Each byte is a pan value (examples: 0 is left pan, 32 is center pan and 64 is right pan).
    pub initial_channel_pan: [u8; 64],

    /// Initial volume of the channels
    /// Each byte is a channel volume (0-64).
    pub initial_channel_volume: [u8; 64],
}

impl ItHeader {
    pub fn load(data: &[u8]) -> Result<(Self, usize), ImportError> {
        let mut r = BinReader::new(data);
        let id_bytes: [u8; 4] = r.read_array()?;
        let song_name_bytes: [u8; 26] = r.read_array()?;
        let rows_per_beat = r.read_u8()?;
        let rows_per_measure = r.read_u8()?;
        let order_number = r.read_u16_le()?;
        let instrument_number = r.read_u16_le()?;
        let sample_number = r.read_u16_le()?;
        let pattern_number = r.read_u16_le()?;
        let created_with_tracker = r.read_u16_le()?;
        let compatible_with_tracker = r.read_u16_le()?;
        let flags = r.read_u16_le()?;
        let special_flags = r.read_u16_le()?;
        let global_volume = r.read_u8()?;
        let mix_volume = r.read_u8()?;
        let initial_speed = r.read_u8()?;
        let initial_bpm = r.read_u8()?;
        let pan_separation = r.read_u8()?;
        let pitch_wheel_depth = r.read_u8()?;
        let message_length = r.read_u16_le()?;
        let message_offset = r.read_u32_le()?;
        let reserved_bytes: [u8; 4] = r.read_array()?;
        let initial_channel_pan: [u8; 64] = r.read_array()?;
        let initial_channel_volume: [u8; 64] = r.read_array()?;

        let header = ItHeader {
            id: bytes_to_trimmed_string(&id_bytes),
            song_name: bytes_to_trimmed_string(&song_name_bytes),
            rows_per_beat,
            rows_per_measure,
            order_number,
            instrument_number,
            sample_number,
            pattern_number,
            created_with_tracker,
            compatible_with_tracker,
            flags,
            special_flags,
            global_volume,
            mix_volume,
            initial_speed,
            initial_bpm,
            pan_separation,
            pitch_wheel_depth,
            message_length,
            message_offset,
            reserved: bytes_to_trimmed_string(&reserved_bytes),
            initial_channel_pan,
            initial_channel_volume,
        };
        if !header.is_it_header() {
            return Err(ImportError::InvalidMagic("Not an IT Module?"));
        }
        Ok((header, IT_HEADER_SIZE))
    }

    pub fn get_size() -> usize {
        IT_HEADER_SIZE
    }

    pub fn is_it_header(&self) -> bool {
        self.id == "IMPM"
    }

    /// Mod Plug file?
    pub fn is_ompt(&self) -> bool {
        self.reserved == "OMPT"
    }

    pub fn is_post20(&self) -> bool {
        (self.created_with_tracker >> 8) >= 2
    }

    // === Configuration Flags ===

    /// Bit 0: Checks if Stereo is on.
    pub fn is_stereo(&self) -> bool {
        (self.flags & (1 << 0)) != 0
    }

    /// Bit 1: Checks if Mixing is on (obsolete).
    pub fn is_mixing(&self) -> bool {
        (self.flags & (1 << 1)) != 0
    }

    /// Bit 2: Checks if Instruments are used.
    pub fn is_instruments_used(&self) -> bool {
        (self.flags & (1 << 2)) != 0
    }

    /// Bit 3: Checks if Linear slides are used.
    pub fn is_linear_slides(&self) -> bool {
        (self.flags & (1 << 3)) != 0
    }

    /// Bit 4: Checks if Old effects are used.
    pub fn is_old_effects(&self) -> bool {
        (self.flags & (1 << 4)) != 0
    }

    /// Bit 5: Checks if G effect is linked with E and F memory.
    pub fn is_g_linked_with_e_f(&self) -> bool {
        (self.flags & (1 << 5)) != 0
    }

    /// Bit 6: Checks if MIDI pitch is controlled.
    pub fn is_midi_pitch_controlled(&self) -> bool {
        (self.flags & (1 << 6)) != 0
    }

    /// Bit 7: Checks if Embedded MIDI Macros are requested.
    pub fn is_embedded_midi_macros(&self) -> bool {
        (self.flags & (1 << 7)) != 0
    }

    // === Special Configuration Flags ===

    /// Bit 0: Checks if Song message is attached.
    pub fn is_song_message_attached(&self) -> bool {
        (self.special_flags & (1 << 0)) != 0
    }

    /// Bit 1: Checks if Edit history is embedded.
    pub fn is_edit_history_embedded(&self) -> bool {
        (self.special_flags & (1 << 1)) != 0
    }

    /// Bit 2: Checks if Highlight is embedded.
    pub fn is_highlight_embedded(&self) -> bool {
        (self.special_flags & (1 << 2)) != 0
    }

    /// Bit 3: Checks if Embedded MIDI Macro is present.
    pub fn is_embedded_midi_macro(&self) -> bool {
        (self.special_flags & (1 << 3)) != 0
    }
}
