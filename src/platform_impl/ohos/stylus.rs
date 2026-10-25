//! Convert native degrees and pressure without admitting NaN or invalid ranges.
use openharmony_ability::StylusInputData;

use crate::event::Force;

pub(crate) fn stylus_force(pen: StylusInputData) -> Option<Force> {
    if !pen.pressure.is_finite() {
        return None;
    }
    let pressure = f64::from(pen.pressure.clamp(0.0, 1.0));
    if pen.tilt_x.is_finite()
        && pen.tilt_y.is_finite()
        && (pen.tilt_x != 0.0 || pen.tilt_y != 0.0)
    {
        let tx = f64::from(pen.tilt_x.clamp(-90.0, 90.0)).to_radians();
        let ty = f64::from(pen.tilt_y.clamp(-90.0, 90.0)).to_radians();
        let tan_sq = tx.tan().powi(2) + ty.tan().powi(2);
        let altitude = (1.0 / (1.0 + tan_sq).sqrt()).asin();
        Some(Force::Calibrated {
            force: pressure,
            max_possible_force: 1.0,
            altitude_angle: Some(altitude),
        })
    } else {
        Some(Force::Normalized(pressure))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_pen_units_and_invalid_samples() {
        let force = stylus_force(StylusInputData {
            pressure: 0.25,
            tilt_x: 0.0,
            tilt_y: 0.0,
            rotation: Some(-45.0),
        });
        assert_eq!(force, Some(Force::Normalized(0.25)));

        let invalid = stylus_force(StylusInputData {
            pressure: f32::NAN,
            tilt_x: f32::INFINITY,
            tilt_y: 0.0,
            rotation: Some(f64::NAN),
        });
        assert_eq!(invalid, None);
    }
}
