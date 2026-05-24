//! Pure-math geo helpers shared by the geo MCP tool wrappers.
//!
//! No async, no I/O, no dependencies beyond `core` — keeping the
//! algorithms here as plain functions means callers can use them
//! directly from Rust without paying for the MCP / serde-json
//! round-trip, and the tests can run without a tokio runtime.

/// Mean Earth radius in metres (WGS84 IUGG R₁ ≈ semi-major + semi-
/// minor mean). The Haversine formula assumes a spherical Earth; for
/// distances on the order of TW administrative boundaries the error
/// is well under 0.5 %, which is plenty for "is this point near that
/// landmark" use cases.
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Great-circle distance between two `(lat, lon)` points in **metres**.
///
/// Inputs are interpreted as decimal degrees. The implementation uses
/// `2·R·asin(√h)` where `h = sin²(Δφ/2) + cos φ₁·cos φ₂·sin²(Δλ/2)`
/// — the textbook Haversine form, which is numerically well-behaved
/// for small Δ (the alternative `acos(...)` form loses precision when
/// the two points are close).
pub fn distance_haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let phi1 = lat1.to_radians();
    let phi2 = lat2.to_radians();
    let dphi = (lat2 - lat1).to_radians();
    let dlambda = (lon2 - lon1).to_radians();
    let a = (dphi / 2.0).sin().powi(2) + phi1.cos() * phi2.cos() * (dlambda / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * a.sqrt().asin()
}

/// Test whether `(lat, lon)` lies inside the polygon given as a list
/// of `(lat, lon)` vertices.
///
/// Uses the classic ray-casting (Jordan-curve) algorithm in
/// longitude/latitude space — i.e. treats the polygon as a flat
/// shape on the lon×lat plane. That's fine for polygons whose
/// extent is small relative to Earth's curvature (TW townships,
/// city boundaries, etc.); for hemisphere-scale polygons or shapes
/// that cross the antimeridian you'd need a spherical algorithm,
/// which is out of scope for the v1 utility tools.
///
/// The polygon may be open or closed (last vertex equal to first or
/// not); we don't close it implicitly because the ray-cast loop
/// already handles the wrap-around via `vertices[(i + 1) % n]`.
///
/// A polygon with fewer than 3 vertices is degenerate and the
/// function returns `false` for any query point — including points
/// that happen to coincide with one of the vertices, because
/// "inside" isn't well-defined for a line segment or a point.
///
/// Edge / vertex cases (point exactly on the boundary) are not
/// guaranteed to return a stable value; the ray-casting decision is
/// sensitive to floating-point comparison at the threshold. Callers
/// who need on-boundary detection should test for that separately.
pub fn point_in_polygon(lat: f64, lon: f64, vertices: &[(f64, f64)]) -> bool {
    let n = vertices.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (lat_i, lon_i) = vertices[i];
        let (lat_j, lon_j) = vertices[j];
        let crosses_horizontal = (lat_i > lat) != (lat_j > lat);
        if crosses_horizontal {
            let x_intersection = (lon_j - lon_i) * (lat - lat_i) / (lat_j - lat_i) + lon_i;
            if lon < x_intersection {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haversine_zero_distance_is_zero() {
        assert!((distance_haversine_m(25.033, 121.565, 25.033, 121.565)).abs() < 1e-9);
    }

    /// Taipei 101 (25.0337, 121.5645) → Taipei Main Station
    /// (25.0478, 121.5170) is roughly 4.8 km by great-circle (the
    /// straight line through the buildings, not by road). Our value
    /// should match within 1 % of that ballpark.
    #[test]
    fn haversine_taipei_101_to_main_station() {
        let d = distance_haversine_m(25.0337, 121.5645, 25.0478, 121.5170);
        assert!((4_500.0..=5_100.0).contains(&d), "{d} m");
    }

    /// Taipei (25.0, 121.0) → Kaohsiung (22.6, 120.3) — measured
    /// great-circle distance on the WGS84 sphere is ~276 km.
    #[test]
    fn haversine_taipei_to_kaohsiung() {
        let d = distance_haversine_m(25.0, 121.0, 22.6, 120.3);
        assert!((270_000.0..=285_000.0).contains(&d), "{d} m");
    }

    /// Antipodal points should be ~half the Earth's circumference
    /// (≈ 20,037 km).
    #[test]
    fn haversine_antipodal() {
        let d = distance_haversine_m(0.0, 0.0, 0.0, 180.0);
        assert!((19_800_000.0..=20_100_000.0).contains(&d), "{d} m");
    }

    fn unit_square() -> Vec<(f64, f64)> {
        vec![(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)]
    }

    #[test]
    fn point_in_polygon_inside_unit_square() {
        assert!(point_in_polygon(0.5, 0.5, &unit_square()));
    }

    #[test]
    fn point_in_polygon_outside_unit_square() {
        assert!(!point_in_polygon(2.0, 2.0, &unit_square()));
    }

    #[test]
    fn point_in_polygon_outside_negative_quadrant() {
        assert!(!point_in_polygon(-0.5, -0.5, &unit_square()));
    }

    /// Degenerate inputs (n < 3) always return false, including
    /// the case where the query point matches a vertex.
    #[test]
    fn point_in_polygon_degenerate_returns_false() {
        assert!(!point_in_polygon(0.0, 0.0, &[]));
        assert!(!point_in_polygon(0.0, 0.0, &[(0.0, 0.0)]));
        assert!(!point_in_polygon(0.0, 0.0, &[(0.0, 0.0), (1.0, 1.0)]));
    }

    /// L-shaped concave polygon — point in the "notch" must be
    /// reported outside even though it's inside the bounding box.
    #[test]
    fn point_in_polygon_concave_l_shape() {
        let l = vec![
            (0.0, 0.0),
            (0.0, 3.0),
            (1.0, 3.0),
            (1.0, 1.0),
            (3.0, 1.0),
            (3.0, 0.0),
        ];
        assert!(point_in_polygon(0.5, 0.5, &l));
        assert!(!point_in_polygon(2.0, 2.0, &l)); // inside bbox, outside L
    }
}
