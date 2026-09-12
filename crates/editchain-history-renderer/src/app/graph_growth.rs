//! New connections grow from their existing parent toward their new endpoint.
//! Only changed, visible SVG segments participate; independent branches run in parallel.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Point {
    pub(super) x: i64,
    pub(super) y: i64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Segment {
    pub(super) start: Point,
    pub(super) end: Point,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Timing {
    pub(super) delay: f64,
    pub(super) duration: f64,
}

#[derive(Debug, Default)]
pub(super) struct Growth {
    pub(super) segments: Vec<Timing>,
    pub(super) arrivals: HashMap<Point, f64>,
}

pub(super) fn plan(segments: &[Segment]) -> Growth {
    let mut adjacent: HashMap<Point, Vec<usize>> = HashMap::new();
    for (index, segment) in segments.iter().enumerate() {
        adjacent.entry(segment.start).or_default().push(index);
        adjacent.entry(segment.end).or_default().push(index);
    }
    let mut growth = Growth {
        segments: vec![Timing::default(); segments.len()],
        ..Growth::default()
    };
    let mut visited = HashSet::new();
    for seed in 0..segments.len() {
        if visited.contains(&seed) {
            continue;
        }
        let mut pending = vec![seed];
        let mut component = Vec::new();
        while let Some(index) = pending.pop() {
            if !visited.insert(index) {
                continue;
            }
            let Some(segment) = segments.get(index) else {
                continue;
            };
            component.push((index, *segment));
            for point in [segment.start, segment.end] {
                pending.extend(adjacent.get(&point).into_iter().flatten().copied());
            }
        }
        grow_component(&component, &mut growth);
    }
    growth
}

fn grow_component(component: &[(usize, Segment)], growth: &mut Growth) {
    let mut ordered = component.to_vec();
    ordered.sort_by_key(|(_, segment)| std::cmp::Reverse(segment.end.y));
    let mut distance: HashMap<Point, f64> = HashMap::new();
    let mut total = 1.0_f64;
    for (index, segment) in &ordered {
        let length = super::i64_to_f64(segment.end.x.saturating_sub(segment.start.x))
            .hypot(super::i64_to_f64(
                segment.end.y.saturating_sub(segment.start.y),
            ))
            .max(1.0);
        let delay = distance.get(&segment.end).copied().unwrap_or(0.0);
        let finish = delay + length;
        let arrival = distance.entry(segment.start).or_default();
        *arrival = arrival.max(finish);
        total = total.max(finish);
        if let Some(timing) = growth.segments.get_mut(*index) {
            *timing = Timing {
                delay,
                duration: length,
            };
        }
    }
    for (index, _) in ordered {
        if let Some(timing) = growth.segments.get_mut(index) {
            timing.delay *= 520.0 / total;
            timing.duration *= 520.0 / total;
        }
    }
    growth.arrivals.extend(
        distance
            .into_iter()
            .map(|(point, distance)| (point, distance * 520.0 / total)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(start: (i64, i64), end: (i64, i64)) -> Segment {
        Segment {
            start: Point {
                x: start.0,
                y: start.1,
            },
            end: Point { x: end.0, y: end.1 },
        }
    }

    #[test]
    fn connection_grows_continuously_from_parent_through_each_row_seam() {
        let growth = plan(&[
            segment((0, 0), (0, 10)),
            segment((0, 10), (0, 20)),
            segment((0, 20), (0, 30)),
        ]);
        let top = growth.segments.first().unwrap();
        let middle = growth.segments.get(1).unwrap();
        let bottom = growth.segments.get(2).unwrap();
        assert!(bottom.delay.abs() < 0.01);
        assert!((bottom.duration - middle.delay).abs() < 0.01);
        assert!((middle.delay + middle.duration - top.delay).abs() < 0.01);
        assert!((top.delay + top.duration - 520.0).abs() < 0.01);
    }

    #[test]
    fn forks_and_merges_join_at_the_endpoint_while_separate_connections_grow_in_parallel() {
        let growth = plan(&[
            segment((10, 0), (0, 10)),
            segment((10, 0), (20, 10)),
            segment((10, -10), (10, 0)),
            segment((100, 0), (100, 1000)),
        ]);
        let left = growth.segments.first().unwrap();
        let right = growth.segments.get(1).unwrap();
        let trunk = growth.segments.get(2).unwrap();
        let independent = growth.segments.get(3).unwrap();
        assert!((left.duration - right.duration).abs() < 0.01);
        assert!((trunk.delay - left.duration).abs() < 0.01);
        assert!((trunk.delay + trunk.duration - 520.0).abs() < 0.01);
        assert!(independent.delay.abs() < 0.01);
        assert!((independent.duration - 520.0).abs() < 0.01);
    }
}
