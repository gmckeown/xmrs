pub use crate::import::patternslot::PatternSlot;
use crate::pitch::Pitch;

impl PatternSlot {
    fn amiga_pitch(period: u16) -> Option<u8> {
        match period {
            6848 => Some(1),
            6464 => Some(2),
            6096 => Some(3),
            5760 => Some(4),
            5424 => Some(5),
            5120 => Some(6),
            4832 => Some(7),
            4560 => Some(8),
            4304 => Some(9),
            4064 => Some(10),
            3840 => Some(11),
            3624 => Some(12),
            3424 => Some(13),
            3232 => Some(14),
            3048 => Some(15),
            2880 => Some(16),
            2712 => Some(17),
            2560 => Some(18),
            2416 => Some(19),
            2280 => Some(20),
            2152 => Some(21),
            2032 => Some(22),
            1920 => Some(23),
            1812 => Some(24),
            1712 => Some(25),
            1616 => Some(26),
            1524 => Some(27),
            1440 => Some(28),
            1356 => Some(29),
            1280 => Some(30),
            1208 => Some(31),
            1140 => Some(32),
            1076 => Some(33),
            1016 => Some(34),
            960 => Some(35),
            906 => Some(36),
            856 => Some(37),
            808 => Some(38),
            762 => Some(39),
            720 => Some(40),
            678 => Some(41),
            640 => Some(42),
            604 => Some(43),
            570 => Some(44),
            538 => Some(45),
            508 => Some(46),
            480 => Some(47),
            453 => Some(48),
            428 => Some(49),
            404 => Some(50),
            381 => Some(51),
            360 => Some(52),
            339 => Some(53),
            320 => Some(54),
            302 => Some(55),
            285 => Some(56),
            269 => Some(57),
            254 => Some(58),
            240 => Some(59),
            226 => Some(60),
            214 => Some(61),
            202 => Some(62),
            190 => Some(63),
            180 => Some(64),
            170 => Some(65),
            160 => Some(66),
            151 => Some(67),
            143 => Some(68),
            135 => Some(69),
            127 => Some(70),
            120 => Some(71),
            113 => Some(72),
            107 => Some(73),
            101 => Some(74),
            95 => Some(75),
            90 => Some(76),
            85 => Some(77),
            80 => Some(78),
            75 => Some(79),
            71 => Some(80),
            67 => Some(81),
            63 => Some(82),
            60 => Some(83),
            56 => Some(84),
            0 => Some(0),
            _ => None,
        }
    }

    /*
        0bIIII_PPPPPPPPPPPP_IIII_EEEE_DDDDDDDD
        P: period
        I: instrument number
        E: effect number
        D: effect data
    */
    pub fn deserialize(input: u32) -> Self {
        let period = ((input >> 16) & 0x0FFF) as u16;
        let instrument_high = ((input >> (32 - 4)) & 0x000F) as u8;
        let instrument_low = ((input >> 12) & 0x000F) as u8;
        let instrument = (instrument_high << 4) | instrument_low;
        let effect = ((input >> 8) & 0x000F) as u8;
        let data = (input & 0x00FF) as u8;

        // TODO: one day do better error handling for notes here
        let nu8 = match Self::amiga_pitch(period) {
            Some(n) => n,
            None => {
                // period is not an Amiga one?
                0
            }
        };

        let note: Pitch = if nu8 == 0 {
            Pitch::None
        } else {
            Pitch::try_from(nu8 - 1).unwrap_or(Pitch::None)
        };

        let instrument = if instrument == 0 {
            None
        } else {
            Some(instrument as usize - 1)
        };

        Self {
            note,
            instrument,
            volume: 0,
            effect_type: effect,
            effect_parameter: data,
        }
    }
}
