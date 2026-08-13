//! Vanilla moon phases and their contribution to local difficulty.

/// The number of ticks for which one moon phase remains active.
pub const MOON_PHASE_LENGTH_TICKS: i64 = 24_000;

/// Vanilla's moon brightness lookup, indexed by [`MoonPhase::index`].
pub const MOON_BRIGHTNESS_PER_PHASE: [f32; MoonPhase::COUNT] =
    [1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75];

/// A phase in Vanilla's eight-phase moon cycle.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum MoonPhase {
    /// A fully illuminated moon.
    #[default]
    FullMoon = 0,
    /// The waning phase between a full moon and third quarter.
    WaningGibbous = 1,
    /// The waning half moon.
    ThirdQuarter = 2,
    /// The waning phase before a new moon.
    WaningCrescent = 3,
    /// A moon with no visible illumination.
    NewMoon = 4,
    /// The waxing phase after a new moon.
    WaxingCrescent = 5,
    /// The waxing half moon.
    FirstQuarter = 6,
    /// The waxing phase before a full moon.
    WaxingGibbous = 7,
}

impl MoonPhase {
    /// The number of phases in Vanilla's moon cycle.
    pub const COUNT: usize = 8;

    /// Returns the phase index used by Vanilla's brightness lookup.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Returns this phase's Vanilla moon brightness.
    #[must_use]
    pub const fn brightness(self) -> f32 {
        MOON_BRIGHTNESS_PER_PHASE[self.index()]
    }

    /// Decodes the value stored in Vanilla's moon timeline.
    #[must_use]
    pub const fn from_timeline_value(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"full_moon" => Some(Self::FullMoon),
            b"waning_gibbous" => Some(Self::WaningGibbous),
            b"third_quarter" => Some(Self::ThirdQuarter),
            b"waning_crescent" => Some(Self::WaningCrescent),
            b"new_moon" => Some(Self::NewMoon),
            b"waxing_crescent" => Some(Self::WaxingCrescent),
            b"first_quarter" => Some(Self::FirstQuarter),
            b"waxing_gibbous" => Some(Self::WaxingGibbous),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_MOON_BRIGHTNESS: f32 = 1.0;
    const QUARTER_MOON_BRIGHTNESS: f32 = 0.5;
    const NEW_MOON_BRIGHTNESS: f32 = 0.0;

    #[test]
    fn timeline_values_map_to_vanilla_phase_order() {
        let phases = [
            ("full_moon", MoonPhase::FullMoon),
            ("waning_gibbous", MoonPhase::WaningGibbous),
            ("third_quarter", MoonPhase::ThirdQuarter),
            ("waning_crescent", MoonPhase::WaningCrescent),
            ("new_moon", MoonPhase::NewMoon),
            ("waxing_crescent", MoonPhase::WaxingCrescent),
            ("first_quarter", MoonPhase::FirstQuarter),
            ("waxing_gibbous", MoonPhase::WaxingGibbous),
        ];

        for (expected_index, (value, expected_phase)) in phases.into_iter().enumerate() {
            assert_eq!(MoonPhase::from_timeline_value(value), Some(expected_phase));
            assert_eq!(expected_phase.index(), expected_index);
        }
    }

    #[test]
    fn phase_brightness_matches_vanilla_lookup() {
        assert_eq!(MoonPhase::FullMoon.brightness(), FULL_MOON_BRIGHTNESS);
        assert_eq!(
            MoonPhase::ThirdQuarter.brightness(),
            QUARTER_MOON_BRIGHTNESS
        );
        assert_eq!(MoonPhase::NewMoon.brightness(), NEW_MOON_BRIGHTNESS);
        assert_eq!(
            MoonPhase::FirstQuarter.brightness(),
            QUARTER_MOON_BRIGHTNESS
        );
    }
}
