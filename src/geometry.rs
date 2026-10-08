use crate::model::{Bounds, Point};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GeometryError {
    TooFewVertices,
    DuplicateAdjacentVertices,
    ZeroArea,
    SelfIntersection,
}

impl std::fmt::Display for GeometryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::TooFewVertices => "polygon must contain at least three vertices",
            Self::DuplicateAdjacentVertices => "polygon cannot contain adjacent duplicate vertices",
            Self::ZeroArea => "polygon must enclose a non-zero area",
            Self::SelfIntersection => {
                "polygon must be simple and cannot self-intersect or touch itself"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for GeometryError {}

pub fn validate_simple_polygon(vertices: &[Point]) -> Result<(), GeometryError> {
    if vertices.len() < 3 {
        return Err(GeometryError::TooFewVertices);
    }
    if vertices
        .iter()
        .enumerate()
        .any(|(index, point)| *point == vertices[(index + 1) % vertices.len()])
    {
        return Err(GeometryError::DuplicateAdjacentVertices);
    }
    if signed_double_area(vertices) == 0 {
        return Err(GeometryError::ZeroArea);
    }
    for first in 0..vertices.len() {
        let first_next = (first + 1) % vertices.len();
        for second in (first + 1)..vertices.len() {
            let second_next = (second + 1) % vertices.len();
            if first == second || first_next == second || second_next == first {
                continue;
            }
            if segments_intersect(
                vertices[first],
                vertices[first_next],
                vertices[second],
                vertices[second_next],
            ) {
                return Err(GeometryError::SelfIntersection);
            }
        }
    }
    Ok(())
}

pub fn point_in_or_on_polygon(point: Point, polygon: &[Point]) -> bool {
    let mut inside = false;
    for index in 0..polygon.len() {
        let first = polygon[index];
        let second = polygon[(index + 1) % polygon.len()];
        if point_on_segment(first, second, point) {
            return true;
        }
        let crosses_horizontal_ray = (first.y_mm > point.y_mm) != (second.y_mm > point.y_mm);
        if !crosses_horizontal_ray {
            continue;
        }
        let side = orientation(first, second, point);
        if (second.y_mm > first.y_mm && side > 0) || (second.y_mm < first.y_mm && side < 0) {
            inside = !inside;
        }
    }
    inside
}

pub fn segment_hits_polygon(start: Point, end: Point, polygon: &[Point]) -> bool {
    if point_in_or_on_polygon(start, polygon) || point_in_or_on_polygon(end, polygon) {
        return true;
    }
    (0..polygon.len()).any(|index| {
        segments_intersect(
            start,
            end,
            polygon[index],
            polygon[(index + 1) % polygon.len()],
        )
    })
}

pub fn segment_stays_in_bounds(start: Point, end: Point, bounds: &Bounds) -> bool {
    bounds.contains(start) && bounds.contains(end)
}

fn signed_double_area(vertices: &[Point]) -> i128 {
    (0..vertices.len())
        .map(|index| {
            let first = vertices[index];
            let second = vertices[(index + 1) % vertices.len()];
            i128::from(first.x_mm) * i128::from(second.y_mm)
                - i128::from(first.y_mm) * i128::from(second.x_mm)
        })
        .sum()
}

fn orientation(first: Point, second: Point, third: Point) -> i128 {
    (i128::from(second.x_mm) - i128::from(first.x_mm))
        * (i128::from(third.y_mm) - i128::from(first.y_mm))
        - (i128::from(second.y_mm) - i128::from(first.y_mm))
            * (i128::from(third.x_mm) - i128::from(first.x_mm))
}

fn point_on_segment(start: Point, end: Point, point: Point) -> bool {
    orientation(start, end, point) == 0
        && point.x_mm >= start.x_mm.min(end.x_mm)
        && point.x_mm <= start.x_mm.max(end.x_mm)
        && point.y_mm >= start.y_mm.min(end.y_mm)
        && point.y_mm <= start.y_mm.max(end.y_mm)
}

fn segments_intersect(
    first_start: Point,
    first_end: Point,
    second_start: Point,
    second_end: Point,
) -> bool {
    let first_a = orientation(first_start, first_end, second_start);
    let first_b = orientation(first_start, first_end, second_end);
    let second_a = orientation(second_start, second_end, first_start);
    let second_b = orientation(second_start, second_end, first_end);
    if first_a == 0 && point_on_segment(first_start, first_end, second_start) {
        return true;
    }
    if first_b == 0 && point_on_segment(first_start, first_end, second_end) {
        return true;
    }
    if second_a == 0 && point_on_segment(second_start, second_end, first_start) {
        return true;
    }
    if second_b == 0 && point_on_segment(second_start, second_end, first_end) {
        return true;
    }
    (first_a.is_positive() != first_b.is_positive())
        && (second_a.is_positive() != second_b.is_positive())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Vec<Point> {
        vec![
            Point { x_mm: 2, y_mm: 2 },
            Point { x_mm: 4, y_mm: 2 },
            Point { x_mm: 4, y_mm: 4 },
            Point { x_mm: 2, y_mm: 4 },
        ]
    }

    #[test]
    fn polygon_validation_rejects_crossing_edges() {
        let bow_tie = vec![
            Point { x_mm: 0, y_mm: 0 },
            Point { x_mm: 4, y_mm: 4 },
            Point { x_mm: 0, y_mm: 4 },
            Point { x_mm: 4, y_mm: 0 },
            Point { x_mm: 5, y_mm: 5 },
        ];
        assert_eq!(
            validate_simple_polygon(&bow_tie),
            Err(GeometryError::SelfIntersection)
        );
    }

    #[test]
    fn polygon_boundary_and_swept_crossing_are_inclusive() {
        let polygon = square();
        assert!(point_in_or_on_polygon(Point { x_mm: 2, y_mm: 3 }, &polygon));
        assert!(point_in_or_on_polygon(Point { x_mm: 3, y_mm: 3 }, &polygon));
        assert!(segment_hits_polygon(
            Point { x_mm: 0, y_mm: 3 },
            Point { x_mm: 6, y_mm: 3 },
            &polygon,
        ));
        assert!(!segment_hits_polygon(
            Point { x_mm: 0, y_mm: 0 },
            Point { x_mm: 1, y_mm: 0 },
            &polygon,
        ));
    }

    #[test]
    fn polygon_validation_accepts_simple_polygon() {
        assert_eq!(validate_simple_polygon(&square()), Ok(()));
    }
}
