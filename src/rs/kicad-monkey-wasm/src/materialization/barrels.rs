//! The drilled holes of a pack, for an analytic renderer: each as a stadium
//! (a round drill is one of length 0) with its plating (none for an unplated
//! hole, which is then only a wall), copper span, and backdrills, in board
//! millimetres (KiCad frame, y down).

use kicad_monkey_contracts::generated::materialization_document::{
    Affine2D, BoardSide, HoleOperation, HolePlating, NmPoint, Shape2d,
};

/// Floats per barrel: centre x, centre y, half the straight length, the long
/// axis' angle from +x (radians), radius, plating thickness, and two zeros.
pub const BARREL_FLOATS: usize = 8;
/// Words per barrel: operation index, start and end slice orders, the order a
/// front and a back backdrill run until (`NONE` without), flags, two zeros.
pub const BARREL_WORDS: usize = 8;
/// No backdrill from that side.
pub const NONE: u32 = u32::MAX;
/// Flag: the plating thickness is the default, not the pack's.
pub const FLAG_DEFAULT_PLATING: u32 = 1;
/// Flag: an unplated hole (its plating is 0).
pub const FLAG_UNPLATED: u32 = 2;

/// Plating assumed when a hole carries none (IPC class 2's minimum average).
pub const DEFAULT_PLATING_NM: f64 = 25_000.0;

/// The holes, `BARREL_FLOATS` floats and `BARREL_WORDS` words each, in the
/// holes' order; and how many had a shape a barrel cannot take.
#[derive(Debug, Default)]
pub struct Barrels {
    pub floats: Vec<f32>,
    pub words: Vec<u32>,
    pub skipped: u32,
}

fn point(value: &NmPoint) -> (f64, f64) {
    let coordinate = |index: usize| value.0.get(index).map_or(0.0, |n| f64::from(n.0));
    (coordinate(0), coordinate(1))
}

fn apply(affine: &Affine2D, (x, y): (f64, f64)) -> (f64, f64) {
    (
        affine.m11 * x + affine.m12 * y + f64::from(affine.tx_nm.0),
        affine.m21 * x + affine.m22 * y + f64::from(affine.ty_nm.0),
    )
}

/// The hole's stadium in board nanometres: centre, half length, angle, radius.
fn stadium(hole: &HoleOperation) -> Option<((f64, f64), f64, f64, f64)> {
    let scale = (hole.affine.m11 * hole.affine.m22 - hole.affine.m12 * hole.affine.m21)
        .abs()
        .sqrt();
    match &hole.shape {
        Shape2d::Disk2d(disk) => Some((
            apply(&hole.affine, point(&disk.center_nm)),
            0.0,
            0.0,
            *disk.radius_nm as f64 * scale,
        )),
        Shape2d::Capsule2d(capsule) => {
            let start = apply(&hole.affine, point(&capsule.start_nm));
            let end = apply(&hole.affine, point(&capsule.end_nm));
            let (dx, dy) = (end.0 - start.0, end.1 - start.1);
            Some((
                ((start.0 + end.0) / 2.0, (start.1 + end.1) / 2.0),
                dx.hypot(dy) / 2.0,
                dy.atan2(dx),
                *capsule.radius_nm as f64 * scale,
            ))
        }
        _ => None,
    }
}

/// Every hole's barrel (unplated ones flagged); `orders` gives each slice
/// key's order.
pub fn barrels(holes: &[HoleOperation], orders: &impl Fn(&str) -> Option<u32>) -> Barrels {
    let mut out = Barrels::default();
    for hole in holes {
        let plated = hole.plating == HolePlating::Plated;
        let (Some(start), Some(end)) = (orders(&hole.start_slice_key), orders(&hole.end_slice_key))
        else {
            out.skipped += 1;
            continue;
        };
        let Some(((cx, cy), half, angle, radius)) = stadium(hole) else {
            out.skipped += 1;
            continue;
        };
        let plating = if plated {
            hole.plating_thickness_nm.as_ref().map(|nm| **nm as f64)
        } else {
            Some(0.0)
        };
        let until = |side: BoardSide| {
            hole.backdrills
                .iter()
                .filter(|drill| drill.side == side)
                .filter_map(|drill| drill.until_slice_key.as_deref().and_then(orders))
                .next()
                .unwrap_or(NONE)
        };
        let mm = |nm: f64| (nm / 1e6) as f32;
        out.floats.extend([
            mm(cx),
            mm(cy),
            mm(half),
            angle as f32,
            mm(radius),
            mm(plating.unwrap_or(DEFAULT_PLATING_NM)),
            0.0,
            0.0,
        ]);
        out.words.extend([
            hole.index,
            start,
            end,
            until(BoardSide::Front),
            until(BoardSide::Back),
            u32::from(plating.is_none()) * FLAG_DEFAULT_PLATING
                + u32::from(!plated) * FLAG_UNPLATED,
            0,
            0,
        ]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hole(json: &str) -> HoleOperation {
        serde_json::from_str(json).expect("hole")
    }

    const AFFINE: &str =
        r#"{"m11":0.0,"m12":-1.0,"m21":1.0,"m22":0.0,"tx_nm":1000000,"ty_nm":2000000}"#;

    fn base(shape: &str, extra: &str) -> String {
        format!(
            r#"{{"kind":"hole","index":7,"affine":{AFFINE},"bounds":{{"min_x_nm":0,"min_y_nm":0,"max_x_nm":1,"max_y_nm":1}},
            "backdrills":[],"post_machining":[],"plating":"plated","start_slice_key":"copper.front","end_slice_key":"copper.back",
            "shape":{shape}{extra}}}"#
        )
    }

    fn orders(key: &str) -> Option<u32> {
        match key {
            "copper.front" => Some(0),
            "copper.in1" => Some(2),
            "copper.back" => Some(4),
            _ => None,
        }
    }

    #[test]
    fn a_slot_turns_with_its_affine_and_keeps_its_plating() {
        let slot = hole(&base(
            r#"{"kind":"capsule_2d","start_nm":[-400000,0],"end_nm":[400000,0],"radius_nm":300000}"#,
            r#","plating_thickness_nm":20000"#,
        ));
        let out = barrels(&[slot], &orders);
        assert_eq!(out.skipped, 0);
        let f = &out.floats;
        // A quarter turn: the slot runs along y, centred on the translation.
        assert!((f[0] - 1.0).abs() < 1e-6 && (f[1] - 2.0).abs() < 1e-6);
        assert!((f[2] - 0.4).abs() < 1e-6);
        assert!((f[3] - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert!((f[4] - 0.3).abs() < 1e-6 && (f[5] - 0.02).abs() < 1e-6);
        assert_eq!(&out.words[..6], &[7, 0, 4, NONE, NONE, 0]);
    }

    #[test]
    fn round_drills_default_their_plating_and_report_backdrills() {
        let via = hole(
            &base(
                r#"{"kind":"disk_2d","center_nm":[0,0],"radius_nm":100000}"#,
                "",
            )
            .replace(
                r#""backdrills":[]"#,
                r#""backdrills":[{"side":"back","until_slice_key":"copper.in1"}]"#,
            ),
        );
        let unplated = hole(
            &base(
                r#"{"kind":"disk_2d","center_nm":[0,0],"radius_nm":100000}"#,
                "",
            )
            .replace(r#""plating":"plated""#, r#""plating":"unplated""#),
        );
        let out = barrels(&[via, unplated], &orders);
        assert_eq!(out.floats.len(), 2 * BARREL_FLOATS);
        assert_eq!(out.floats[2], 0.0);
        assert!((out.floats[5] - 0.025).abs() < 1e-6);
        assert_eq!(&out.words[..6], &[7, 0, 4, NONE, 2, FLAG_DEFAULT_PLATING]);
        // An unplated hole is a wall only: no plating, flagged.
        assert_eq!(out.floats[BARREL_FLOATS + 5], 0.0);
        assert_eq!(out.words[BARREL_WORDS + 5], FLAG_UNPLATED);
    }
}
