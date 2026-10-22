use ruffle_render::shape_utils::calculate_shape_bounds;
use swf::{Point, PointDelta, Shape, ShapeRecord, StyleChangeData, Twips};

pub const FAUX_ITALIC: u8 = 1;
pub const FAUX_BOLD: u8 = 2;

// GFxFontCacheManager::GFxFontCacheManager @eee580 decomp
const FAUX_BOLD_RATIO: f32 = f32::from_bits(0x3d3851ec);
// GFxFontCacheManager::GFxFontCacheManager @eee580 decomp
const FAUX_ITALIC_ANGLE: f32 = f32::from_bits(0x3e8f5c29);
// GCompoundShape::AddCurve @fa1850, GStrokeVertexType::CalcDistance decomp
const EPSILON: f32 = 1.0e-4;
// GFxFontCacheManagerImpl::copyAndTransformShape @eef2e0 decomp
const CURVE_TOLERANCE: f32 = 10.0;
// GFxFontCacheManagerImpl::GFxFontCacheManagerImpl @eedc50 decomp
const MITER_LIMIT: f32 = 4.0;
// GFxFontCacheManagerImpl::copyAndTransformShape @eef2e0 decomp
const STRETCH: f32 = 1000.0;

#[derive(Clone, Copy)]
struct Vertex {
    x: f32,
    y: f32,
    dist: f32,
}

impl Vertex {
    fn new(x: f32, y: f32) -> Self {
        Self { x, y, dist: 0.0 }
    }

    // GStrokeVertexType::CalcDistance decomp
    fn calc_distance(&mut self, other: &Vertex) -> bool {
        self.dist = ((other.x - self.x) * (other.x - self.x) + (other.y - self.y) * (other.y - self.y)).sqrt();
        self.dist > EPSILON
    }
}

type Path = Vec<(f32, f32)>;

// GCompoundShape::flattenQuadraticCurve @fa15e0 decomp
#[expect(clippy::too_many_arguments)]
fn flatten_quadratic(path: &mut Path, tolerance: f32, mut x1: f32, mut y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) {
    let mut cx1 = (x2 + x1) * 0.5;
    let mut cy1 = (y2 + y1) * 0.5;
    let mut dx = x3 - x1;
    let mut dy = y3 - y1;
    let mut d = (dy * (x2 - x3) - dx * (y2 - y3)).abs();
    let mut cx2 = (x3 + x2) * 0.5;
    let mut cy2 = (y3 + y2) * 0.5;
    while EPSILON < d {
        if d * d <= tolerance * (dx * dx + dy * dy) {
            break;
        }
        let mx = (cx1 + cx2) * 0.5;
        let my = (cy1 + cy2) * 0.5;
        flatten_quadratic(path, tolerance, x1, y1, cx1, cy1, mx, my);
        cx1 = (mx + cx2) * 0.5;
        cy1 = (my + cy2) * 0.5;
        dx = x3 - mx;
        dy = y3 - my;
        d = (dy * (cx2 - x3) - dx * (cy2 - y3)).abs();
        x1 = mx;
        y1 = my;
        cx2 = (x3 + cx2) * 0.5;
        cy2 = (y3 + cy2) * 0.5;
    }
    path.push((x3, y3));
}

// GCompoundShape::AddCurve @fa1850 decomp
fn add_curve(path: &mut Path, tolerance: f32, cx: f32, cy: f32, ax: f32, ay: f32) {
    let (px, py) = *path.last().unwrap();
    if ((cy - py) * (ax - cx) - (ay - cy) * (cx - px)).abs() < EPSILON {
        path.push((ax, ay));
        return;
    }
    flatten_quadratic(path, tolerance, px, py, cx, cy, ax, ay);
}

#[derive(Clone, Copy)]
enum Segment {
    Move(f32, f32),
    Line(f32, f32),
    Curve(f32, f32, f32, f32),
}

fn segments(shape: &Shape, unit: f32) -> Vec<Segment> {
    let at = |v: i32| v as f32 / unit;
    let (mut x, mut y) = (0, 0);
    let mut out = Vec::new();
    for record in &shape.shape {
        match record {
            ShapeRecord::StyleChange(change) => {
                if let Some(to) = change.move_to {
                    x = to.x.get();
                    y = to.y.get();
                }
                out.push(Segment::Move(at(x), at(y)));
            }
            ShapeRecord::StraightEdge { delta } => {
                x += delta.dx.get();
                y += delta.dy.get();
                out.push(Segment::Line(at(x), at(y)));
            }
            ShapeRecord::CurvedEdge { control_delta, anchor_delta } => {
                let (cx, cy) = (x + control_delta.dx.get(), y + control_delta.dy.get());
                x = cx + anchor_delta.dx.get();
                y = cy + anchor_delta.dy.get();
                out.push(Segment::Curve(at(cx), at(cy), at(x), at(y)));
            }
        }
    }
    out
}

// GFxShapeBase::MakeCompoundShapeImpl<GFxPathData>, GCompoundShape::SetCurveTolerance @fa0990 decomp
fn compound_shape(segments: &[Segment]) -> Vec<Path> {
    let tolerance = CURVE_TOLERANCE * 0.25 * CURVE_TOLERANCE * 0.25;
    let mut paths: Vec<Path> = Vec::new();
    for segment in segments {
        if let Segment::Move(x, y) = *segment {
            paths.push(vec![(x, y)]);
            continue;
        }
        if paths.is_empty() {
            paths.push(vec![(0.0, 0.0)]);
        }
        let path = paths.last_mut().unwrap();
        match *segment {
            Segment::Line(x, y) => path.push((x, y)),
            Segment::Curve(cx, cy, x, y) => add_curve(path, tolerance, cx, cy, x, y),
            Segment::Move(..) => unreachable!(),
        }
    }
    paths
}

// GFxFontCacheManagerImpl::isOuterContourCW @eec2e0 decomp
fn is_outer_contour_cw(paths: &[Path]) -> bool {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (1.0e10f32, 1.0e10f32, -1.0e10f32, -1.0e10f32);
    let (mut outer_min_x, mut outer_min_y, mut outer_max_x, mut outer_max_y) = (min_x, min_y, max_x, max_y);
    let mut clockwise = true;
    for path in paths.iter().filter(|path| path.len() > 2) {
        let mut area = 0.0f32;
        let (mut px, mut py) = *path.last().unwrap();
        for &(x, y) in path {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            area += y * px - x * py;
            (px, py) = (x, y);
        }
        if min_x < outer_min_x || min_y < outer_min_y || outer_max_x < max_x || outer_max_y < max_y {
            clockwise = 0.0 < area;
            (outer_min_x, outer_min_y, outer_max_x, outer_max_y) = (min_x, min_y, max_x, max_y);
        }
    }
    clockwise
}

struct Stroker {
    width: f32,
    width_abs: f32,
    vertices: Vec<Vertex>,
    closed: bool,
}

impl Stroker {
    fn add(&mut self, x: f32, y: f32) {
        let n = self.vertices.len();
        if n > 1 {
            let next = self.vertices[n - 1];
            if !self.vertices[n - 2].calc_distance(&next) {
                self.vertices.pop();
            }
        }
        self.vertices.push(Vertex::new(x, y));
    }

    // GStrokePath::ClosePath @f98e90 decomp
    fn close_path(&mut self) {
        let v = &mut self.vertices;
        self.closed = false;
        if !v.is_empty() {
            let n = v.len();
            if n > 1 {
                let last = v[n - 1];
                if !v[n - 2].calc_distance(&last) {
                    v.pop();
                }
            }
            v.push(v[0]);
        }
        if v.len() > 1 {
            let first = v[0];
            let n = v.len();
            self.closed = !v[n - 1].calc_distance(&first);
            while v.len() > 1 {
                let n = v.len();
                let last = v[n - 1];
                if v[n - 2].calc_distance(&last) {
                    break;
                }
                v.pop();
                v.pop();
                v.push(last);
            }
        }
        if self.closed {
            while v.len() > 2 {
                let first = v[0];
                let n = v.len();
                if v[n - 1].calc_distance(&first) {
                    break;
                }
                v.pop();
            }
            if v.len() < 3 {
                self.closed = false;
            }
        }
    }

    // GStroker::calcMiter @f992f0 decomp
    #[expect(clippy::too_many_arguments)]
    fn calc_miter(
        &self,
        out: &mut Path,
        v0: Vertex,
        v1: Vertex,
        v2: Vertex,
        dx1: f32,
        dy1: f32,
        dx2: f32,
        dy2: f32,
        revert: bool,
        limit: f32,
        epsilon: f32,
        bevel: f32,
    ) {
        let mut distance = 1.0f32;
        let mut limit_exceeded = true;
        let mut intersection_failed = true;
        let (ax1, ay1) = (dx1 + v0.x, dy1 + v0.y);
        let ax2 = v1.x + dx1;
        let (bx1, by1) = (dx2 + v1.x, dy2 + v1.y);
        let by2_by1 = (dy2 + v2.y) - by1;
        let ay2_ay1 = (v1.y + dy1) - ay1;
        let bx2_bx1 = (v2.x + dx2) - bx1;
        let denominator = by2_by1 * (ax2 - ax1) - bx2_bx1 * ay2_ay1;
        let (mut xi, mut yi) = (0.0, 0.0);
        if epsilon <= denominator.abs() {
            let r = ((ay1 - by1) * bx2_bx1 - (ax1 - bx1) * by2_by1) / denominator;
            xi = r * (ax2 - ax1) + ax1;
            yi = ay1 + r * ay2_ay1;
            distance = ((xi - v1.x) * (xi - v1.x) + (yi - v1.y) * (yi - v1.y)).sqrt();
            if distance <= self.width_abs * limit {
                out.push((xi, yi));
                limit_exceeded = false;
            }
            intersection_failed = false;
            if !limit_exceeded {
                return;
            }
        } else {
            let y = v1.y + dy1;
            if (dy1 * (v0.x - ax2) - dx1 * (v0.y - y) < 0.0) != (dy1 * (v2.x - ax2) - dx1 * (v2.y - y) < 0.0) {
                out.push((ax2, y));
                return;
            }
        }
        let (end_x, end_y);
        if revert {
            out.push((v1.x + dx1, v1.y + dy1));
            end_x = dx2 + v1.x;
            end_y = dy2 + v1.y;
        } else if intersection_failed {
            out.push(((v1.x + dx1) - dy1 * limit, v1.y + dy1 + limit * dx1));
            end_x = dy2 * limit + v1.x + dx2;
            end_y = (v1.y + dy2) - limit * dx2;
        } else {
            let t = (self.width_abs * limit - bevel) / (distance - bevel);
            out.push((t * (xi - (v1.x + dx1)) + v1.x + dx1, v1.y + dy1 + t * (yi - (v1.y + dy1))));
            end_x = dx2 + v1.x + t * (xi - (dx2 + v1.x));
            end_y = dy2 + v1.y + (yi - (dy2 + v1.y)) * t;
        }
        out.push((end_x, end_y));
    }

    // GStroker::calcJoin @f99bc0 decomp
    fn calc_join(&self, out: &mut Path, v0: Vertex, v1: Vertex, v2: Vertex, len1: f32, len2: f32) {
        let epsilon = (len2 + len1) * 0.01;
        let dx1 = (self.width_abs * (v1.y - v0.y)) / len1;
        let dy1 = ((v0.x - v1.x) * self.width_abs) / len1;
        let dx2 = (self.width_abs * (v2.y - v1.y)) / len2;
        let dy2 = ((v1.x - v2.x) * self.width_abs) / len2;
        if 0.0 < (v2.x - v1.x) * (v1.y - v0.y) - (v1.x - v0.x) * (v2.y - v1.y) {
            let limit = len1.min(len2) / self.width_abs;
            self.calc_miter(out, v0, v1, v2, dx1, dy1, dx2, dy2, true, limit, epsilon, 0.0);
            return;
        }
        let (dx, dy) = ((dx1 + dx2) * 0.5, (dy1 + dy2) * 0.5);
        let bevel = (dx * dx + dy * dy).sqrt();
        self.calc_miter(out, v0, v1, v2, dx1, dy1, dx2, dy2, false, MITER_LIMIT, epsilon, bevel);
    }

    // GStroker::generateEquidistant(GCompoundShape &, bool) @f9a040 decomp
    fn generate(&self, out: &mut Vec<Path>) {
        let v = &self.vertices;
        let n = v.len();
        if n < 2 || !self.closed {
            return;
        }
        let mut path = Path::new();
        if 0.0 < self.width {
            for i in 0..n {
                let prev = if i == 0 { n - 1 } else { i - 1 };
                let next = (i + 1) % n;
                self.calc_join(&mut path, v[prev], v[i], v[next], v[prev].dist, v[i].dist);
            }
        } else {
            for i in (0..n).rev() {
                let prev = if i == 0 { n - 1 } else { i - 1 };
                let next = (i + 1) % n;
                self.calc_join(&mut path, v[next], v[i], v[prev], v[i].dist, v[prev].dist);
            }
        }
        out.push(path);
    }
}

// GStroker::generateEquidistant(GCompoundShape const &, int, GCompoundShape &, bool) @f9a540 decomp
fn equidistant(paths: &[Path], width: f32) -> Vec<Path> {
    let mut stroker = Stroker {
        width,
        width_abs: width.abs(),
        vertices: Vec::new(),
        closed: false,
    };
    let mut out = Vec::new();
    for path in paths.iter().filter(|path| !path.is_empty()) {
        let mut from = 0;
        if let Some(last) = stroker.vertices.last() {
            let (x, y) = path[0];
            if ((x - last.x) * (x - last.x) + (y - last.y) * (y - last.y)).sqrt() <= EPSILON {
                from = 1;
            } else {
                stroker.close_path();
                stroker.generate(&mut out);
                stroker.vertices.clear();
            }
        }
        for &(x, y) in &path[from..] {
            stroker.add(x, y);
        }
    }
    if stroker.vertices.len() > 1 {
        stroker.close_path();
        stroker.generate(&mut out);
    }
    out
}

fn shape_from_segments(source: &Shape, segments: &[Segment], unit: f32) -> Shape {
    let fill = source.shape.iter().find_map(|record| match record {
        ShapeRecord::StyleChange(change) => Some((change.fill_style_0, change.fill_style_1)),
        _ => None,
    });
    let (fill_style_0, fill_style_1) = fill.unwrap_or((None, None));
    let twips = |v: f32| Twips::new(((v as i32) as f32 * unit) as i32);
    let (mut x, mut y) = (Twips::ZERO, Twips::ZERO);
    let mut records = Vec::new();
    for segment in segments {
        match *segment {
            Segment::Move(mx, my) => {
                (x, y) = (twips(mx), twips(my));
                records.push(ShapeRecord::StyleChange(Box::new(StyleChangeData {
                    move_to: Some(Point::new(x, y)),
                    fill_style_0,
                    fill_style_1,
                    line_style: None,
                    new_styles: None,
                })));
            }
            Segment::Line(lx, ly) => {
                let (nx, ny) = (twips(lx), twips(ly));
                records.push(ShapeRecord::StraightEdge {
                    delta: PointDelta::new(nx - x, ny - y),
                });
                (x, y) = (nx, ny);
            }
            Segment::Curve(cx, cy, ax, ay) => {
                let (cx, cy) = (twips(cx), twips(cy));
                let (nx, ny) = (twips(ax), twips(ay));
                records.push(ShapeRecord::CurvedEdge {
                    control_delta: PointDelta::new(cx - x, cy - y),
                    anchor_delta: PointDelta::new(nx - cx, ny - cy),
                });
                (x, y) = (nx, ny);
            }
        }
    }
    let bounds = calculate_shape_bounds(&records);
    Shape {
        version: source.version,
        id: source.id,
        shape_bounds: bounds.clone(),
        edge_bounds: bounds,
        flags: source.flags,
        styles: source.styles.clone(),
        shape: records,
    }
}

// GFxFontCacheManagerImpl::copyAndTransformShape @eef2e0, GFxFontCacheManagerImpl::SetFauxItalicAngle @eeb120, GMatrix2D::Transform @ebbed0 decomp
pub fn faux_shape(source: &Shape, faux: u8, units_per_em: f32) -> Shape {
    let unit = units_per_em / 1024.0;
    let mut outline = segments(source, unit);
    if faux & FAUX_BOLD != 0 {
        let mut paths = compound_shape(&outline);
        for path in &mut paths {
            for point in path {
                point.1 *= STRETCH;
            }
        }
        let mut width = FAUX_BOLD_RATIO * 1024.0;
        if !is_outer_contour_cw(&paths) {
            width = -width;
        }
        width *= 0.5;
        outline = Vec::new();
        for path in equidistant(&paths, width).into_iter().filter(|path| path.len() > 2) {
            let mut points = path.into_iter().map(|(x, y)| ((x as i32) as f32, ((y * 0.001) as i32) as f32));
            let (x, y) = points.next().unwrap();
            outline.push(Segment::Move(x, y));
            outline.extend(points.map(|(x, y)| Segment::Line(x, y)));
        }
    }
    if faux & FAUX_ITALIC != 0 {
        let shear = (-FAUX_ITALIC_ANGLE).tan();
        for segment in &mut outline {
            *segment = match *segment {
                Segment::Move(x, y) => Segment::Move(x + shear * y, y),
                Segment::Line(x, y) => Segment::Line(x + shear * y, y),
                Segment::Curve(cx, cy, x, y) => Segment::Curve(cx + shear * cy, cy, x + shear * y, y),
            };
        }
    }
    shape_from_segments(source, &outline, unit)
}
