use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Weak};

use eframe::egui;
use egui::epaint::text::{FontFamily, FontId, LayoutJob, TextFormat};
use egui::{Color32, Id, Pos2, Rect, Response, Sense, Stroke, Vec2};
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};

use crate::document::math::{make_font_id, offscreen_fonts_2x, MAX_CANVAS_PIXELS, RgbaCanvas};
use crate::ui::theme::{ThemeMode, WordTheme};

/// Built-in starter templates offered in the WYSIWYG Mermaid toolbar.
pub const MERMAID_TEMPLATES: &[(&str, &str)] = &[
    (
        "Flowchart",
        "flowchart LR\n    A([Start]) --> B{Decision}\n    B -->|Yes| C[Process]\n    B -->|No| D([End])\n    C --> D",
    ),
    (
        "Sequence",
        "sequenceDiagram\n    participant U as User\n    participant A as API\n    participant D as Database\n    U->>+A: Request data\n    A->>+D: Query rows\n    D-->>-A: Result set\n    A-->>-U: JSON response",
    ),
    (
        "State",
        "stateDiagram-v2\n    [*] --> Idle\n    Idle --> Loading : fetch\n    Loading --> Ready : success\n    Loading --> Error : fail\n    Error --> Idle : retry\n    Ready --> [*]",
    ),
    (
        "Class",
        "classDiagram\n    class Document {\n        +String title\n        +save()\n    }\n    class MarkdownDoc {\n        +parseAst()\n        +exportPdf()\n    }\n    Document <|-- MarkdownDoc : extends",
    ),
    (
        "ER Diagram",
        "erDiagram\n    USER ||--o{ DOCUMENT : owns\n    USER {\n        int id PK\n        string name\n    }\n    DOCUMENT {\n        int id PK\n        int user_id FK\n        string title\n    }",
    ),
    (
        "Pie Chart",
        "pie showData\n    title Project Breakdown\n    \"Core Engine\" : 45\n    \"UI & WYSIWYG\" : 30\n    \"Export & Tests\" : 25",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeLineStyle {
    Solid,
    Dotted,
    Thick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneTextAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone)]
pub enum SceneItem {
    RoundedRect {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        radius: f32,
        fill: Option<[u8; 4]>,
        stroke: Option<([u8; 4], f32)>,
        dashed: bool,
    },
    Polygon {
        pts: Vec<(f32, f32)>,
        fill: Option<[u8; 4]>,
        stroke: Option<([u8; 4], f32)>,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        fill: Option<[u8; 4]>,
        stroke: Option<([u8; 4], f32)>,
    },
    Line {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        stroke: [u8; 4],
        width: f32,
        style: EdgeLineStyle,
    },
    Polyline {
        pts: Vec<(f32, f32)>,
        stroke: [u8; 4],
        width: f32,
        style: EdgeLineStyle,
    },
    Cylinder {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        fill: [u8; 4],
        stroke: ([u8; 4], f32),
    },
    Text {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        bold: bool,
        italic: bool,
        color: [u8; 4],
        align: SceneTextAlign,
    },
}

#[derive(Debug, Clone)]
pub struct DiagramScene {
    pub kind_label: &'static str,
    pub width: f32,
    pub height: f32,
    pub bg: [u8; 4],
    pub items: Vec<SceneItem>,
}

#[derive(Debug, Clone, Copy)]
pub struct DiagramPalette {
    pub bg: [u8; 4],
    pub node_fill: [u8; 4],
    pub node_alt_fill: [u8; 4],
    pub node_stroke: [u8; 4],
    pub subgraph_fill: [u8; 4],
    pub subgraph_stroke: [u8; 4],
    pub header_fill: [u8; 4],
    pub edge_color: [u8; 4],
    pub pill_bg: [u8; 4],
    pub note_fill: [u8; 4],
    pub note_stroke: [u8; 4],
    pub text_primary: [u8; 4],
    pub text_muted: [u8; 4],
    pub accent: [u8; 4],
    pub pie_colors: [[u8; 4]; 8],
}

fn c32_arr(c: Color32) -> [u8; 4] {
    [c.r(), c.g(), c.b(), c.a()]
}

impl DiagramPalette {
    pub fn from_theme(theme: &WordTheme) -> Self {
        if theme.dark {
            Self {
                bg: c32_arr(theme.code_bg),
                node_fill: [40, 48, 62, 255],
                node_alt_fill: [48, 58, 76, 255],
                node_stroke: [118, 165, 240, 255],
                subgraph_fill: [30, 35, 45, 220],
                subgraph_stroke: [78, 92, 118, 255],
                header_fill: [52, 68, 96, 255],
                edge_color: [170, 186, 212, 255],
                pill_bg: [34, 40, 52, 245],
                note_fill: [64, 58, 36, 255],
                note_stroke: [188, 162, 74, 255],
                text_primary: c32_arr(theme.text_primary),
                text_muted: c32_arr(theme.text_muted),
                accent: c32_arr(theme.accent),
                pie_colors: [
                    [96, 165, 250, 255],
                    [52, 211, 153, 255],
                    [251, 191, 36, 255],
                    [244, 114, 182, 255],
                    [167, 139, 250, 255],
                    [248, 113, 113, 255],
                    [45, 212, 191, 255],
                    [251, 146, 60, 255],
                ],
            }
        } else {
            Self {
                bg: c32_arr(theme.paper_bg),
                node_fill: [241, 246, 254, 255],
                node_alt_fill: [232, 240, 254, 255],
                node_stroke: c32_arr(theme.accent),
                subgraph_fill: [248, 250, 252, 240],
                subgraph_stroke: [168, 184, 206, 255],
                header_fill: [219, 234, 254, 255],
                edge_color: [71, 85, 105, 255],
                pill_bg: [255, 255, 255, 242],
                note_fill: [254, 249, 195, 255],
                note_stroke: [202, 138, 4, 255],
                text_primary: c32_arr(theme.text_primary),
                text_muted: c32_arr(theme.text_muted),
                accent: c32_arr(theme.accent),
                pie_colors: [
                    [37, 99, 235, 255],
                    [16, 185, 129, 255],
                    [245, 158, 11, 255],
                    [236, 72, 153, 255],
                    [139, 92, 246, 255],
                    [239, 68, 68, 255],
                    [20, 184, 166, 255],
                    [249, 115, 22, 255],
                ],
            }
        }
    }

    pub fn export_light() -> Self {
        Self::from_theme(&WordTheme::light())
    }
}

// ---------------------------------------------------------------------------
// Text Measurement Abstraction
// ---------------------------------------------------------------------------

trait DiagramTextMeasurer {
    fn measure_block(&mut self, text: &str, size: f32, bold: bool) -> (f32, f32);
}

struct EguiDiagramMeasurer<'a>(&'a egui::Context);
impl DiagramTextMeasurer for EguiDiagramMeasurer<'_> {
    fn measure_block(&mut self, text: &str, size: f32, bold: bool) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, size * 1.25);
        }
        let family = if bold {
            FontFamily::Name("bold".into())
        } else {
            FontFamily::Proportional
        };
        let mut job = LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: FontId::new(size, family),
                color: Color32::BLACK,
                ..Default::default()
            },
        );
        let g = self.0.fonts(|f| f.layout_job(job));
        (g.size().x, g.size().y.max(size * 1.2))
    }
}

struct OffscreenDiagramMeasurer<'a>(&'a egui::epaint::text::Fonts);
impl DiagramTextMeasurer for OffscreenDiagramMeasurer<'_> {
    fn measure_block(&mut self, text: &str, size: f32, bold: bool) -> (f32, f32) {
        if text.is_empty() {
            return (0.0, size * 1.25);
        }
        let mut job = LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: make_font_id(size, bold, false),
                color: Color32::BLACK,
                ..Default::default()
            },
        );
        let g = self.0.layout_job(job);
        (g.size().x, g.size().y.max(size * 1.2))
    }
}

// ---------------------------------------------------------------------------
// Shared Preprocessing & Color Parsing Helpers
// ---------------------------------------------------------------------------

fn clean_mermaid_label(raw: &str) -> String {
    let t = raw.trim();
    let unquoted = if (t.starts_with('"') && t.ends_with('"') && t.len() >= 2)
        || (t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2)
    {
        &t[1..t.len() - 1]
    } else {
        t
    };
    unquoted
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<br>", "\n")
        .replace("\\n", "\n")
        .trim()
        .to_string()
}

fn preprocess_mermaid_lines(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_init = false;
    for raw_line in code.lines() {
        let mut s = raw_line.trim();
        if in_init {
            if let Some(end_idx) = s.find("}%%") {
                s = s[end_idx + 3..].trim();
                in_init = false;
            } else {
                continue;
            }
        }
        if let Some(start_init) = s.find("%%{") {
            if let Some(end_init) = s[start_init + 3..].find("}%%") {
                let combined = format!("{}{}", &s[..start_init], &s[start_init + 3 + end_init + 3..]);
                let trimmed = combined.trim();
                if !trimmed.is_empty() {
                    split_semicolon_statements(trimmed, &mut out);
                }
                continue;
            } else {
                in_init = true;
                continue;
            }
        }
        if let Some(cmt) = find_comment_outside_quotes(s) {
            s = s[..cmt].trim();
        }
        if !s.is_empty() {
            split_semicolon_statements(s, &mut out);
        }
    }
    out
}

fn find_comment_outside_quotes(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut in_dq = false;
    let mut in_sq = false;
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        match bytes[i] {
            b'"' if !in_sq => in_dq = !in_dq,
            b'\'' if !in_dq => in_sq = !in_sq,
            b'%' if !in_dq && !in_sq && bytes[i + 1] == b'%' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn split_semicolon_statements(line: &str, out: &mut Vec<String>) {
    let mut in_dq = false;
    let mut in_sq = false;
    let mut start = 0usize;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' if !in_sq => in_dq = !in_dq,
            '\'' if !in_dq => in_sq = !in_sq,
            ';' if !in_dq && !in_sq => {
                let seg = line[start..i].trim();
                if !seg.is_empty() {
                    out.push(seg.to_string());
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = line[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
}

fn parse_css_color(val: &str) -> Option<[u8; 4]> {
    let t = val.trim();
    if let Some(hex) = t.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some([r, g, b, 255]);
        }
        if hex.len() == 3 {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()? * 17;
            return Some([r, g, b, 255]);
        }
    }
    match t.to_ascii_lowercase().as_str() {
        "red" => Some([239, 68, 68, 255]),
        "green" => Some([34, 197, 94, 255]),
        "blue" => Some([59, 130, 246, 255]),
        "yellow" => Some([250, 204, 21, 255]),
        "orange" => Some([249, 115, 22, 255]),
        "purple" => Some([168, 85, 247, 255]),
        "gray" | "grey" => Some([148, 163, 184, 255]),
        "white" => Some([255, 255, 255, 255]),
        "black" => Some([15, 23, 42, 255]),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Generic Layered Graph Layout Engine (Flowchart + State + Class + ER)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphDirection {
    TopDown,
    BottomUp,
    LeftRight,
    RightLeft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowShape {
    Rectangle,
    Rounded,
    Stadium,
    Subroutine,
    Cylinder,
    Circle,
    DoubleCircle,
    Diamond,
    Hexagon,
    Parallelogram,
    ParallelogramAlt,
    Trapezoid,
    TrapezoidAlt,
    Asymmetric,
    StateStart,
    StateEnd,
    ForkBar,
    ClassBox,
    EntityBox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeEndpointKind {
    None,
    Arrow,
    Cross,
    Circle,
    HollowTriangle,
    FilledDiamond,
    HollowDiamond,
    ErOne,
    ErZeroOrOne,
    ErMany,
    ErZeroOrMany,
}

#[derive(Debug, Clone)]
struct ClassBoxData {
    stereotype: Option<String>,
    title: String,
    attributes: Vec<String>,
    methods: Vec<String>,
}

#[derive(Debug, Clone)]
struct EntityBoxData {
    title: String,
    fields: Vec<(String, String, String)>, // (type, name, key)
}

#[derive(Debug, Clone)]
struct GraphNode {
    label: String,
    shape: FlowShape,
    fill_override: Option<[u8; 4]>,
    stroke_override: Option<[u8; 4]>,
    text_override: Option<[u8; 4]>,
    subgraph_id: Option<usize>,
    class_box: Option<ClassBoxData>,
    entity_box: Option<EntityBoxData>,
    // Layout computed fields
    w: f32,
    h: f32,
    cx: f32,
    cy: f32,
    layer: usize,
    order: usize,
}

#[derive(Debug, Clone)]
struct GraphEdge {
    from: usize,
    to: usize,
    label: String,
    style: EdgeLineStyle,
    start_marker: EdgeEndpointKind,
    end_marker: EdgeEndpointKind,
    start_label: String,
    end_label: String,
}

#[derive(Debug, Clone)]
struct SubgraphInfo {
    title: String,
    parent: Option<usize>,
}

type NodeStyleTriple = (Option<[u8; 4]>, Option<[u8; 4]>, Option<[u8; 4]>);

struct GraphModel {
    direction: GraphDirection,
    nodes: Vec<GraphNode>,
    node_map: HashMap<String, usize>,
    edges: Vec<GraphEdge>,
    subgraphs: Vec<SubgraphInfo>,
    class_defs: HashMap<String, NodeStyleTriple>,
}

impl GraphModel {
    fn new(direction: GraphDirection) -> Self {
        Self {
            direction,
            nodes: Vec::new(),
            node_map: HashMap::new(),
            edges: Vec::new(),
            subgraphs: Vec::new(),
            class_defs: HashMap::new(),
        }
    }

    fn ensure_node(&mut self, id: &str, active_subgraph: Option<usize>) -> usize {
        if let Some(&idx) = self.node_map.get(id) {
            if self.nodes[idx].subgraph_id.is_none() && active_subgraph.is_some() {
                self.nodes[idx].subgraph_id = active_subgraph;
            }
            return idx;
        }
        let idx = self.nodes.len();
        self.nodes.push(GraphNode {
            label: id.to_string(),
            shape: FlowShape::Rectangle,
            fill_override: None,
            stroke_override: None,
            text_override: None,
            subgraph_id: active_subgraph,
            class_box: None,
            entity_box: None,
            w: 80.0,
            h: 40.0,
            cx: 0.0,
            cy: 0.0,
            layer: 0,
            order: 0,
        });
        self.node_map.insert(id.to_string(), idx);
        idx
    }
}

struct ClassBoxLayout {
    w: f32,
    h: f32,
    head_h: f32,
    stereotype_h: f32,
    attr_heights: Vec<f32>,
    meth_heights: Vec<f32>,
    sep_y_rel: Option<f32>,
    meth_top_rel: f32,
}

fn compute_class_box_layout(cb: &ClassBoxData, measurer: &mut dyn DiagramTextMeasurer) -> ClassBoxLayout {
    let (tw, th) = measurer.measure_block(&cb.title, 13.0, true);
    let mut max_w = tw + 28.0;
    let stereotype_h = if let Some(ref st) = cb.stereotype {
        let (sw, sh) = measurer.measure_block(st, 10.0, false);
        max_w = max_w.max(sw + 26.0);
        sh
    } else {
        0.0
    };
    let head_h = if cb.stereotype.is_some() {
        (th + stereotype_h + 18.0).max(42.0)
    } else {
        (th + 16.0).max(32.0)
    };

    let mut attr_heights = Vec::with_capacity(cb.attributes.len());
    let mut attr_block_h = 0.0_f32;
    if !cb.attributes.is_empty() {
        attr_block_h += 8.0;
        for a in &cb.attributes {
            let (aw, ah) = measurer.measure_block(a, 11.5, false);
            max_w = max_w.max(aw + 22.0);
            attr_heights.push(ah);
            attr_block_h += ah + 4.0;
        }
        attr_block_h += 4.0;
    }

    let mut meth_heights = Vec::with_capacity(cb.methods.len());
    let mut meth_block_h = 0.0_f32;
    if !cb.methods.is_empty() {
        meth_block_h += 8.0;
        for m in &cb.methods {
            let (mw, mh) = measurer.measure_block(m, 11.5, false);
            max_w = max_w.max(mw + 22.0);
            meth_heights.push(mh);
            meth_block_h += mh + 4.0;
        }
        meth_block_h += 6.0;
    }

    let sep_y_rel = if !cb.attributes.is_empty() && !cb.methods.is_empty() {
        Some(head_h + attr_block_h)
    } else {
        None
    };
    let meth_top_rel = head_h + attr_block_h + 8.0;
    let body_h = if cb.attributes.is_empty() && cb.methods.is_empty() {
        12.0
    } else if cb.methods.is_empty() {
        attr_block_h + 2.0
    } else {
        attr_block_h + meth_block_h
    };

    ClassBoxLayout {
        w: max_w.max(120.0),
        h: (head_h + body_h).max(46.0),
        head_h,
        stereotype_h,
        attr_heights,
        meth_heights,
        sep_y_rel,
        meth_top_rel,
    }
}

struct EntityBoxLayout {
    w: f32,
    h: f32,
    head_h: f32,
    row_texts: Vec<(String, f32)>,
}

fn compute_entity_box_layout(eb: &EntityBoxData, measurer: &mut dyn DiagramTextMeasurer) -> EntityBoxLayout {
    let (tw, th) = measurer.measure_block(&eb.title, 13.0, true);
    let mut max_w = tw + 30.0;
    let head_h = (th + 16.0).max(32.0);
    let mut row_texts = Vec::with_capacity(eb.fields.len());
    let mut fields_h = 0.0_f32;
    for (ty, name, key) in &eb.fields {
        let line = if key.is_empty() {
            format!("{ty}  {name}")
        } else {
            format!("[{key}]  {ty}  {name}")
        };
        let (rw, rh) = measurer.measure_block(&line, 11.5, false);
        max_w = max_w.max(rw + 24.0);
        fields_h += rh + 5.0;
        row_texts.push((line, rh));
    }
    let body_h = if eb.fields.is_empty() {
        12.0
    } else {
        8.0 + fields_h + 5.0
    };
    EntityBoxLayout {
        w: max_w.max(120.0),
        h: (head_h + body_h).max(44.0),
        head_h,
        row_texts,
    }
}

fn measure_graph_nodes(graph: &mut GraphModel, measurer: &mut dyn DiagramTextMeasurer) {
    for node in &mut graph.nodes {
        match node.shape {
            FlowShape::StateStart => {
                node.w = 22.0;
                node.h = 22.0;
            }
            FlowShape::StateEnd => {
                node.w = 26.0;
                node.h = 26.0;
            }
            FlowShape::ForkBar => {
                if matches!(graph.direction, GraphDirection::LeftRight | GraphDirection::RightLeft) {
                    node.w = 10.0;
                    node.h = 68.0;
                } else {
                    node.w = 78.0;
                    node.h = 10.0;
                }
            }
            FlowShape::ClassBox => {
                if let Some(ref cb) = node.class_box {
                    let lay = compute_class_box_layout(cb, measurer);
                    node.w = lay.w;
                    node.h = lay.h;
                }
            }
            FlowShape::EntityBox => {
                if let Some(ref eb) = node.entity_box {
                    let lay = compute_entity_box_layout(eb, measurer);
                    node.w = lay.w;
                    node.h = lay.h;
                }
            }
            _ => {
                let (tw, th) = measurer.measure_block(&node.label, 12.5, false);
                match node.shape {
                    FlowShape::Circle | FlowShape::DoubleCircle => {
                        let d = tw.max(th) + 28.0;
                        node.w = d.max(46.0);
                        node.h = d.max(46.0);
                    }
                    FlowShape::Diamond => {
                        node.w = (tw * 1.55 + 38.0).max(76.0);
                        node.h = (th * 1.55 + 32.0).max(58.0);
                    }
                    FlowShape::Hexagon => {
                        node.w = (tw + 44.0).max(82.0);
                        node.h = (th + 22.0).max(40.0);
                    }
                    FlowShape::Cylinder => {
                        node.w = (tw + 32.0).max(74.0);
                        node.h = (th + 30.0).max(52.0);
                    }
                    FlowShape::Parallelogram
                    | FlowShape::ParallelogramAlt
                    | FlowShape::Trapezoid
                    | FlowShape::TrapezoidAlt
                    | FlowShape::Subroutine
                    | FlowShape::Asymmetric => {
                        node.w = (tw + 40.0).max(84.0);
                        node.h = (th + 20.0).max(38.0);
                    }
                    FlowShape::Stadium => {
                        node.w = (tw + 34.0).max(76.0);
                        node.h = (th + 18.0).max(36.0);
                    }
                    FlowShape::Rectangle | FlowShape::Rounded => {
                        node.w = (tw + 28.0).max(72.0);
                        node.h = (th + 18.0).max(36.0);
                    }
                    FlowShape::StateStart
                    | FlowShape::StateEnd
                    | FlowShape::ForkBar
                    | FlowShape::ClassBox
                    | FlowShape::EntityBox => {}
                }
            }
        }
    }
}

fn layout_layered_graph(graph: &mut GraphModel, max_label_w: f32) {
    let n = graph.nodes.len();
    if n == 0 {
        return;
    }

    // 1. Build adjacency and break cycles via iterative DFS back-edge detection for clean layering
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut in_deg = vec![0usize; n];
    let mut seen_edges = FxHashSet::default();
    for e in &graph.edges {
        if e.from != e.to && seen_edges.insert((e.from, e.to)) {
            adj[e.from].push(e.to);
            in_deg[e.to] += 1;
        }
    }

    let mut state = vec![0u8; n]; // 0=unvisited, 1=visiting, 2=done
    let mut dag_adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut dag_in = vec![0usize; n];
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let run_iter_dfs = |start: usize,
                        adj: &[Vec<usize>],
                        state: &mut [u8],
                        dag_adj: &mut [Vec<usize>],
                        dag_in: &mut [usize],
                        stack: &mut Vec<(usize, usize)>| {
        state[start] = 1;
        stack.push((start, 0));
        while let Some((u, idx)) = stack.last_mut() {
            let u = *u;
            if *idx < adj[u].len() {
                let v = adj[u][*idx];
                *idx += 1;
                if state[v] == 0 {
                    dag_adj[u].push(v);
                    dag_in[v] += 1;
                    state[v] = 1;
                    stack.push((v, 0));
                } else if state[v] == 2 {
                    dag_adj[u].push(v);
                    dag_in[v] += 1;
                }
            } else {
                state[u] = 2;
                stack.pop();
            }
        }
    };
    for i in 0..n {
        if in_deg[i] == 0 && state[i] == 0 {
            run_iter_dfs(i, &adj, &mut state, &mut dag_adj, &mut dag_in, &mut stack);
        }
    }
    for i in 0..n {
        if state[i] == 0 {
            run_iter_dfs(i, &adj, &mut state, &mut dag_adj, &mut dag_in, &mut stack);
        }
    }

    // 2. Longest-path layer assignment on DAG
    let mut queue = VecDeque::new();
    let mut layer = vec![0usize; n];
    let mut rem_in = dag_in.clone();
    for (i, &deg) in rem_in.iter().enumerate() {
        if deg == 0 {
            queue.push_back(i);
        }
    }
    while let Some(u) = queue.pop_front() {
        for &v in &dag_adj[u] {
            layer[v] = layer[v].max(layer[u] + 1);
            rem_in[v] = rem_in[v].saturating_sub(1);
            if rem_in[v] == 0 {
                queue.push_back(v);
            }
        }
    }

    let max_layer = layer.iter().copied().max().unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); max_layer + 1];
    for (i, &l) in layer.iter().enumerate() {
        graph.nodes[i].layer = l;
        layers[l].push(i);
    }

    // 3. Barycenter ordering sweep to minimize crossings
    let mut pred: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in &graph.edges {
        if e.from != e.to {
            pred[e.to].push(e.from);
        }
    }
    for (l_idx, l_nodes) in layers.iter().enumerate() {
        for (ord, &nid) in l_nodes.iter().enumerate() {
            graph.nodes[nid].order = ord;
        }
        let _ = l_idx;
    }
    for _pass in 0..4 {
        for layer in layers.iter_mut().take(max_layer + 1).skip(1) {
            let mut scored: Vec<(f32, usize)> = layer
                .iter()
                .map(|&nid| {
                    let ps = &pred[nid];
                    if ps.is_empty() {
                        (graph.nodes[nid].order as f32, nid)
                    } else {
                        let sum: f32 = ps.iter().map(|&p| graph.nodes[p].order as f32).sum();
                        (sum / ps.len() as f32, nid)
                    }
                })
                .collect();
            scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            *layer = scored.into_iter().map(|(_, id)| id).collect();
            for (ord, &nid) in layer.iter().enumerate() {
                graph.nodes[nid].order = ord;
            }
        }
        for l_idx in (0..max_layer).rev() {
            let mut scored: Vec<(f32, usize)> = layers[l_idx]
                .iter()
                .map(|&nid| {
                    let ss = &adj[nid];
                    if ss.is_empty() {
                        (graph.nodes[nid].order as f32, nid)
                    } else {
                        let sum: f32 = ss.iter().map(|&s| graph.nodes[s].order as f32).sum();
                        (sum / ss.len() as f32, nid)
                    }
                })
                .collect();
            scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            layers[l_idx] = scored.into_iter().map(|(_, id)| id).collect();
            for (ord, &nid) in layers[l_idx].iter().enumerate() {
                graph.nodes[nid].order = ord;
            }
        }
    }

    // 4. Coordinate placement along primary axis + secondary cross axis
    let horiz = matches!(graph.direction, GraphDirection::LeftRight | GraphDirection::RightLeft);
    let reversed = matches!(graph.direction, GraphDirection::BottomUp | GraphDirection::RightLeft);
    let has_labels = graph.edges.iter().any(|e| !e.label.is_empty());
    let has_er_markers = graph.edges.iter().any(|e| {
        matches!(
            e.start_marker,
            EdgeEndpointKind::ErOne
                | EdgeEndpointKind::ErZeroOrOne
                | EdgeEndpointKind::ErMany
                | EdgeEndpointKind::ErZeroOrMany
        ) || matches!(
            e.end_marker,
            EdgeEndpointKind::ErOne
                | EdgeEndpointKind::ErZeroOrOne
                | EdgeEndpointKind::ErMany
                | EdgeEndpointKind::ErZeroOrMany
        )
    });
    let layer_gap = if horiz {
        if has_er_markers {
            (max_label_w + 72.0).max(114.0)
        } else if has_labels {
            (max_label_w + 46.0).max(86.0)
        } else {
            58.0
        }
    } else if has_labels || has_er_markers {
        66.0
    } else {
        48.0
    };
    let sibling_gap = if horiz { 30.0 } else { 34.0 };

    let mut layer_primary_extent = vec![0.0_f32; max_layer + 1];
    let mut layer_cross_extent = vec![0.0_f32; max_layer + 1];
    for (l_idx, l_nodes) in layers.iter().enumerate() {
        let mut max_p = 0.0_f32;
        let mut sum_c = 0.0_f32;
        for (k, &nid) in l_nodes.iter().enumerate() {
            let (p_sz, c_sz) = if horiz {
                (graph.nodes[nid].w, graph.nodes[nid].h)
            } else {
                (graph.nodes[nid].h, graph.nodes[nid].w)
            };
            max_p = max_p.max(p_sz);
            sum_c += c_sz;
            if k + 1 < l_nodes.len() {
                sum_c += sibling_gap;
            }
        }
        layer_primary_extent[l_idx] = max_p;
        layer_cross_extent[l_idx] = sum_c;
    }

    let max_cross = layer_cross_extent.iter().copied().fold(0.0_f32, f32::max);
    let mut primary_pos = vec![0.0_f32; max_layer + 1];
    let order_iter: Vec<usize> = if reversed {
        (0..=max_layer).rev().collect()
    } else {
        (0..=max_layer).collect()
    };
    let mut cursor_p = 0.0_f32;
    for l_idx in order_iter {
        primary_pos[l_idx] = cursor_p + layer_primary_extent[l_idx] * 0.5;
        cursor_p += layer_primary_extent[l_idx] + layer_gap;
    }

    for (l_idx, l_nodes) in layers.iter().enumerate() {
        let mut cur_c = (max_cross - layer_cross_extent[l_idx]) * 0.5;
        for &nid in l_nodes {
            let c_sz = if horiz {
                graph.nodes[nid].h
            } else {
                graph.nodes[nid].w
            };
            let center_c = cur_c + c_sz * 0.5;
            if horiz {
                graph.nodes[nid].cx = primary_pos[l_idx];
                graph.nodes[nid].cy = center_c;
            } else {
                graph.nodes[nid].cx = center_c;
                graph.nodes[nid].cy = primary_pos[l_idx];
            }
            cur_c += c_sz + sibling_gap;
        }
    }
}

fn boundary_point_on_node(node: &GraphNode, target_x: f32, target_y: f32) -> (f32, f32) {
    let dx = target_x - node.cx;
    let dy = target_y - node.cy;
    if dx.abs() < 1e-4 && dy.abs() < 1e-4 {
        return (node.cx, node.cy);
    }
    let hw = (node.w * 0.5).max(1.0);
    let hh = (node.h * 0.5).max(1.0);

    match node.shape {
        FlowShape::Circle | FlowShape::DoubleCircle | FlowShape::StateStart | FlowShape::StateEnd => {
            let r = hw.min(hh);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            (node.cx + dx / len * r, node.cy + dy / len * r)
        }
        FlowShape::Diamond => {
            let denom = (dx.abs() / hw + dy.abs() / hh).max(1e-4);
            (node.cx + dx / denom, node.cy + dy / denom)
        }
        _ => {
            let tx = if dx.abs() > 1e-4 { hw / dx.abs() } else { f32::INFINITY };
            let ty = if dy.abs() > 1e-4 { hh / dy.abs() } else { f32::INFINITY };
            let t = tx.min(ty);
            (node.cx + dx * t, node.cy + dy * t)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_endpoint_decoration(
    items: &mut Vec<SceneItem>,
    kind: EdgeEndpointKind,
    tip_x: f32,
    tip_y: f32,
    from_x: f32,
    from_y: f32,
    color: [u8; 4],
    bg: [u8; 4],
) {
    let dx = tip_x - from_x;
    let dy = tip_y - from_y;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy;
    let py = ux;

    match kind {
        EdgeEndpointKind::None => {}
        EdgeEndpointKind::Arrow => {
            let sz = 9.0_f32;
            let base_x = tip_x - ux * sz;
            let base_y = tip_y - uy * sz;
            items.push(SceneItem::Polygon {
                pts: vec![
                    (tip_x, tip_y),
                    (base_x + px * 4.5, base_y + py * 4.5),
                    (base_x - px * 4.5, base_y - py * 4.5),
                ],
                fill: Some(color),
                stroke: None,
            });
        }
        EdgeEndpointKind::HollowTriangle => {
            let sz = 11.0_f32;
            let base_x = tip_x - ux * sz;
            let base_y = tip_y - uy * sz;
            items.push(SceneItem::Polygon {
                pts: vec![
                    (tip_x, tip_y),
                    (base_x + px * 5.5, base_y + py * 5.5),
                    (base_x - px * 5.5, base_y - py * 5.5),
                ],
                fill: Some(bg),
                stroke: Some((color, 1.6)),
            });
        }
        EdgeEndpointKind::FilledDiamond | EdgeEndpointKind::HollowDiamond => {
            let sz = 12.0_f32;
            let mid_x = tip_x - ux * (sz * 0.5);
            let mid_y = tip_y - uy * (sz * 0.5);
            let back_x = tip_x - ux * sz;
            let back_y = tip_y - uy * sz;
            let fill = if kind == EdgeEndpointKind::FilledDiamond {
                color
            } else {
                bg
            };
            items.push(SceneItem::Polygon {
                pts: vec![
                    (tip_x, tip_y),
                    (mid_x + px * 4.8, mid_y + py * 4.8),
                    (back_x, back_y),
                    (mid_x - px * 4.8, mid_y - py * 4.8),
                ],
                fill: Some(fill),
                stroke: Some((color, 1.5)),
            });
        }
        EdgeEndpointKind::Circle => {
            items.push(SceneItem::Circle {
                cx: tip_x - ux * 4.5,
                cy: tip_y - uy * 4.5,
                r: 4.0,
                fill: Some(bg),
                stroke: Some((color, 1.6)),
            });
        }
        EdgeEndpointKind::Cross => {
            let c_x = tip_x - ux * 6.0;
            let c_y = tip_y - uy * 6.0;
            items.push(SceneItem::Line {
                x0: c_x - px * 4.5 - ux * 4.5,
                y0: c_y - py * 4.5 - uy * 4.5,
                x1: c_x + px * 4.5 + ux * 4.5,
                y1: c_y + py * 4.5 + uy * 4.5,
                stroke: color,
                width: 1.8,
                style: EdgeLineStyle::Solid,
            });
            items.push(SceneItem::Line {
                x0: c_x + px * 4.5 - ux * 4.5,
                y0: c_y + py * 4.5 - uy * 4.5,
                x1: c_x - px * 4.5 + ux * 4.5,
                y1: c_y - py * 4.5 + uy * 4.5,
                stroke: color,
                width: 1.8,
                style: EdgeLineStyle::Solid,
            });
        }
        EdgeEndpointKind::ErOne => {
            let bar_x = tip_x - ux * 6.0;
            let bar_y = tip_y - uy * 6.0;
            items.push(SceneItem::Line {
                x0: bar_x - px * 5.5,
                y0: bar_y - py * 5.5,
                x1: bar_x + px * 5.5,
                y1: bar_y + py * 5.5,
                stroke: color,
                width: 1.6,
                style: EdgeLineStyle::Solid,
            });
            let bar2_x = tip_x - ux * 10.0;
            let bar2_y = tip_y - uy * 10.0;
            items.push(SceneItem::Line {
                x0: bar2_x - px * 5.5,
                y0: bar2_y - py * 5.5,
                x1: bar2_x + px * 5.5,
                y1: bar2_y + py * 5.5,
                stroke: color,
                width: 1.6,
                style: EdgeLineStyle::Solid,
            });
        }
        EdgeEndpointKind::ErZeroOrOne => {
            let bar_x = tip_x - ux * 5.5;
            let bar_y = tip_y - uy * 5.5;
            items.push(SceneItem::Line {
                x0: bar_x - px * 5.5,
                y0: bar_y - py * 5.5,
                x1: bar_x + px * 5.5,
                y1: bar_y + py * 5.5,
                stroke: color,
                width: 1.6,
                style: EdgeLineStyle::Solid,
            });
            items.push(SceneItem::Circle {
                cx: tip_x - ux * 12.0,
                cy: tip_y - uy * 12.0,
                r: 3.6,
                fill: Some(bg),
                stroke: Some((color, 1.5)),
            });
        }
        EdgeEndpointKind::ErMany => {
            let neck_x = tip_x - ux * 10.0;
            let neck_y = tip_y - uy * 10.0;
            items.push(SceneItem::Line {
                x0: neck_x,
                y0: neck_y,
                x1: tip_x + px * 6.0,
                y1: tip_y + py * 6.0,
                stroke: color,
                width: 1.6,
                style: EdgeLineStyle::Solid,
            });
            items.push(SceneItem::Line {
                x0: neck_x,
                y0: neck_y,
                x1: tip_x - px * 6.0,
                y1: tip_y - py * 6.0,
                stroke: color,
                width: 1.6,
                style: EdgeLineStyle::Solid,
            });
            items.push(SceneItem::Line {
                x0: neck_x - px * 5.0,
                y0: neck_y - py * 5.0,
                x1: neck_x + px * 5.0,
                y1: neck_y + py * 5.0,
                stroke: color,
                width: 1.6,
                style: EdgeLineStyle::Solid,
            });
        }
        EdgeEndpointKind::ErZeroOrMany => {
            let neck_x = tip_x - ux * 10.0;
            let neck_y = tip_y - uy * 10.0;
            items.push(SceneItem::Line {
                x0: neck_x,
                y0: neck_y,
                x1: tip_x + px * 6.0,
                y1: tip_y + py * 6.0,
                stroke: color,
                width: 1.6,
                style: EdgeLineStyle::Solid,
            });
            items.push(SceneItem::Line {
                x0: neck_x,
                y0: neck_y,
                x1: tip_x - px * 6.0,
                y1: tip_y - py * 6.0,
                stroke: color,
                width: 1.6,
                style: EdgeLineStyle::Solid,
            });
            items.push(SceneItem::Circle {
                cx: tip_x - ux * 14.0,
                cy: tip_y - uy * 14.0,
                r: 3.6,
                fill: Some(bg),
                stroke: Some((color, 1.5)),
            });
        }
    }
}

fn marker_extent(kind: EdgeEndpointKind) -> f32 {
    match kind {
        EdgeEndpointKind::None => 0.0,
        EdgeEndpointKind::Arrow | EdgeEndpointKind::Circle => 10.0,
        EdgeEndpointKind::Cross => 11.0,
        EdgeEndpointKind::HollowTriangle
        | EdgeEndpointKind::FilledDiamond
        | EdgeEndpointKind::HollowDiamond
        | EdgeEndpointKind::ErOne => 13.0,
        EdgeEndpointKind::ErZeroOrOne | EdgeEndpointKind::ErMany => 16.0,
        EdgeEndpointKind::ErZeroOrMany => 19.0,
    }
}

fn compute_skip_curve_offset(
    graph: &GraphModel,
    layer_nodes: &[Vec<usize>],
    edge: &GraphEdge,
    nx: f32,
    ny: f32,
    label_dims: (f32, f32),
) -> f32 {
    let n_from = &graph.nodes[edge.from];
    let n_to = &graph.nodes[edge.to];
    let dx_n = n_to.cx - n_from.cx;
    let dy_n = n_to.cy - n_from.cy;
    let dist = (dx_n * dx_n + dy_n * dy_n).sqrt().max(1.0);
    let ux = dx_n / dist;
    let uy = dy_n / dist;
    let horiz = matches!(graph.direction, GraphDirection::LeftRight | GraphDirection::RightLeft);
    let pill_half = if edge.label.is_empty() {
        12.0
    } else {
        0.5 * ((label_dims.0 + 12.0) * nx.abs() + (label_dims.1 + 8.0) * ny.abs()) + 14.0
    };

    let lo_layer = n_from.layer.min(n_to.layer);
    let hi_layer = n_from.layer.max(n_to.layer).min(layer_nodes.len().saturating_sub(1));

    let mut pos_disp = 24.0_f32;
    let mut neg_disp = 24.0_f32;
    for layer_list in &layer_nodes[lo_layer..=hi_layer] {
        for &idx in layer_list {
            if idx == edge.from || idx == edge.to {
                continue;
            }
            let node = &graph.nodes[idx];
            let rx = node.cx - n_from.cx;
            let ry = node.cy - n_from.cy;
            let proj = rx * ux + ry * uy;
            let half_along = 0.5 * (node.w * ux.abs() + node.h * uy.abs()) + 8.0;
            if proj <= -half_along || proj >= dist + half_along {
                continue;
            }
            let perp = rx * nx + ry * ny;
            let half_perp = 0.5 * (node.w * nx.abs() + node.h * ny.abs());
            let pos_clear = perp + half_perp + pill_half;
            if pos_clear > 0.0 {
                pos_disp = pos_disp.max(pos_clear + 6.0);
            }
            let neg_clear = -perp + half_perp + pill_half;
            if neg_clear > 0.0 {
                neg_disp = neg_disp.max(neg_clear + 6.0);
            }
        }
    }

    let prefer_pos = if horiz { ny >= 0.0 } else { nx >= 0.0 };
    let (chosen_sign, chosen_mid_disp) = if (pos_disp - neg_disp).abs() < 2.0 {
        if prefer_pos { (1.0, pos_disp) } else { (-1.0, neg_disp) }
    } else if pos_disp < neg_disp {
        (1.0, pos_disp)
    } else {
        (-1.0, neg_disp)
    };
    chosen_sign * chosen_mid_disp * 2.0
}

fn translate_scene_items(items: &mut [SceneItem], dx: f32, dy: f32) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    for it in items {
        match it {
            SceneItem::RoundedRect { x0, y0, x1, y1, .. }
            | SceneItem::Cylinder { x0, y0, x1, y1, .. } => {
                *x0 += dx;
                *y0 += dy;
                *x1 += dx;
                *y1 += dy;
            }
            SceneItem::Line { x0, y0, x1, y1, .. } => {
                *x0 += dx;
                *y0 += dy;
                *x1 += dx;
                *y1 += dy;
            }
            SceneItem::Polygon { pts, .. } | SceneItem::Polyline { pts, .. } => {
                for p in pts {
                    p.0 += dx;
                    p.1 += dy;
                }
            }
            SceneItem::Circle { cx, cy, .. } => {
                *cx += dx;
                *cy += dy;
            }
            SceneItem::Text { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
        }
    }
}

fn scene_items_bounds(items: &[SceneItem]) -> (f32, f32, f32, f32) {
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut include = |x: f32, y: f32| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    for it in items {
        match it {
            SceneItem::RoundedRect { x0, y0, x1, y1, .. }
            | SceneItem::Cylinder { x0, y0, x1, y1, .. }
            | SceneItem::Line { x0, y0, x1, y1, .. } => {
                include(*x0, *y0);
                include(*x1, *y1);
            }
            SceneItem::Polygon { pts, .. } | SceneItem::Polyline { pts, .. } => {
                for &(px, py) in pts {
                    include(px, py);
                }
            }
            SceneItem::Circle { cx, cy, r, .. } => {
                include(*cx - *r, *cy - *r);
                include(*cx + *r, *cy + *r);
            }
            SceneItem::Text { x, y, .. } => {
                include(*x, *y);
            }
        }
    }
    if min_x.is_finite() {
        (min_x, min_y, max_x, max_y)
    } else {
        (0.0, 0.0, 180.0, 80.0)
    }
}

fn build_graph_scene(
    kind_label: &'static str,
    mut graph: GraphModel,
    pal: DiagramPalette,
    measurer: &mut dyn DiagramTextMeasurer,
) -> DiagramScene {
    measure_graph_nodes(&mut graph, measurer);
    let mut max_label_w = 0.0_f32;
    for e in &graph.edges {
        if !e.label.is_empty() {
            let (lw, _) = measurer.measure_block(&e.label, 11.0, false);
            max_label_w = max_label_w.max(lw);
        }
    }
    layout_layered_graph(&mut graph, max_label_w);

    if graph.nodes.is_empty() {
        return DiagramScene {
            kind_label,
            width: 240.0,
            height: 64.0,
            bg: pal.bg,
            items: vec![SceneItem::Text {
                x: 120.0,
                y: 32.0,
                text: format!("Empty {kind_label}"),
                size: 12.5,
                bold: false,
                italic: true,
                color: pal.text_muted,
                align: SceneTextAlign::Center,
            }],
        };
    }

    // Compute bounding box (including subgraphs)
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for node in &graph.nodes {
        min_x = min_x.min(node.cx - node.w * 0.5);
        min_y = min_y.min(node.cy - node.h * 0.5);
        max_x = max_x.max(node.cx + node.w * 0.5);
        max_y = max_y.max(node.cy + node.h * 0.5);
    }

    let sg_extra_top = if graph.subgraphs.is_empty() { 0.0 } else { 26.0 };
    let margin = 24.0 + if graph.subgraphs.is_empty() { 0.0 } else { 14.0 };
    let dx = margin - min_x;
    let dy = margin + sg_extra_top - min_y;
    for node in &mut graph.nodes {
        node.cx += dx;
        node.cy += dy;
    }

    let mut scene_w = (max_x - min_x + margin * 2.0).max(180.0);
    let mut scene_h = (max_y - min_y + margin * 2.0 + sg_extra_top).max(80.0);
    let mut items = Vec::new();

    // 1. Subgraph containers (outermost -> innermost)
    for sg_idx in 0..graph.subgraphs.len() {
        let mut s_min_x = f32::INFINITY;
        let mut s_min_y = f32::INFINITY;
        let mut s_max_x = f32::NEG_INFINITY;
        let mut s_max_y = f32::NEG_INFINITY;
        let mut has_any = false;
        for node in &graph.nodes {
            let mut cur = node.subgraph_id;
            let mut belongs = false;
            while let Some(id) = cur {
                if id == sg_idx {
                    belongs = true;
                    break;
                }
                cur = graph.subgraphs.get(id).and_then(|s| s.parent);
            }
            if belongs {
                has_any = true;
                s_min_x = s_min_x.min(node.cx - node.w * 0.5);
                s_min_y = s_min_y.min(node.cy - node.h * 0.5);
                s_max_x = s_max_x.max(node.cx + node.w * 0.5);
                s_max_y = s_max_y.max(node.cy + node.h * 0.5);
            }
        }
        if has_any {
            let pad = 16.0;
            let x0 = s_min_x - pad;
            let y0 = s_min_y - pad - 20.0;
            let x1 = s_max_x + pad;
            let y1 = s_max_y + pad;
            scene_w = scene_w.max(x1 + 16.0);
            scene_h = scene_h.max(y1 + 16.0);
            items.push(SceneItem::RoundedRect {
                x0,
                y0,
                x1,
                y1,
                radius: 8.0,
                fill: Some(pal.subgraph_fill),
                stroke: Some((pal.subgraph_stroke, 1.3)),
                dashed: false,
            });
            items.push(SceneItem::Text {
                x: x0 + 10.0,
                y: y0 + 12.0,
                text: graph.subgraphs[sg_idx].title.clone(),
                size: 11.5,
                bold: true,
                italic: false,
                color: pal.text_muted,
                align: SceneTextAlign::Left,
            });
        }
    }

    // 2. Edges & labels (spread parallel edges cleanly with normal offset)
    let max_layer = graph.nodes.iter().map(|n| n.layer).max().unwrap_or(0);
    let mut layer_nodes: Vec<Vec<usize>> = vec![Vec::new(); max_layer + 1];
    for (idx, node) in graph.nodes.iter().enumerate() {
        layer_nodes[node.layer].push(idx);
    }
    let mut pair_counts: HashMap<(usize, usize), usize> = HashMap::new();
    for e in &graph.edges {
        let key = (e.from.min(e.to), e.from.max(e.to));
        *pair_counts.entry(key).or_insert(0) += 1;
    }
    let mut pair_seen: HashMap<(usize, usize), usize> = HashMap::new();

    for edge in &graph.edges {
        let n_from = &graph.nodes[edge.from];
        let n_to = &graph.nodes[edge.to];
        let stroke_w = match edge.style {
            EdgeLineStyle::Thick => 2.8,
            _ => 1.6,
        };

        if edge.from == edge.to {
            // Self-loop
            let sx = n_from.cx + n_from.w * 0.35;
            let sy = n_from.cy - n_from.h * 0.5;
            let ex = n_from.cx + n_from.w * 0.5;
            let ey = n_from.cy;
            let pts = vec![
                (sx, sy),
                (sx + 10.0, sy - 26.0),
                (ex + 28.0, sy - 24.0),
                (ex + 26.0, ey),
                (ex, ey),
            ];
            scene_w = scene_w.max(ex + 46.0);
            items.push(SceneItem::Polyline {
                pts: pts.clone(),
                stroke: pal.edge_color,
                width: stroke_w,
                style: edge.style,
            });
            emit_endpoint_decoration(
                &mut items,
                edge.end_marker,
                ex,
                ey,
                ex + 26.0,
                ey,
                pal.edge_color,
                pal.bg,
            );
            if !edge.label.is_empty() {
                let (lw, lh) = measurer.measure_block(&edge.label, 11.0, false);
                let lx = ex + 30.0 + lw * 0.5;
                let ly = sy - 16.0;
                scene_w = scene_w.max(lx + lw * 0.5 + 14.0);
                items.push(SceneItem::RoundedRect {
                    x0: lx - lw * 0.5 - 5.0,
                    y0: ly - lh * 0.5 - 3.0,
                    x1: lx + lw * 0.5 + 5.0,
                    y1: ly + lh * 0.5 + 3.0,
                    radius: 4.0,
                    fill: Some(pal.pill_bg),
                    stroke: Some((pal.subgraph_stroke, 0.8)),
                    dashed: false,
                });
                items.push(SceneItem::Text {
                    x: lx,
                    y: ly,
                    text: edge.label.clone(),
                    size: 11.0,
                    bold: false,
                    italic: false,
                    color: pal.text_primary,
                    align: SceneTextAlign::Center,
                });
            }
            continue;
        }

        let key = (edge.from.min(edge.to), edge.from.max(edge.to));
        let total_pair = pair_counts.get(&key).copied().unwrap_or(1);
        let idx_in_pair = pair_seen.entry(key).or_insert(0);
        let cur_slot = *idx_in_pair;
        *idx_in_pair += 1;

        let dx_n = n_to.cx - n_from.cx;
        let dy_n = n_to.cy - n_from.cy;
        let dist = (dx_n * dx_n + dy_n * dy_n).sqrt().max(1.0);
        let nx = -dy_n / dist;
        let ny = dx_n / dist;
        let label_dims = if edge.label.is_empty() {
            (0.0, 0.0)
        } else {
            measurer.measure_block(&edge.label, 11.0, false)
        };
        let curve_offset = if total_pair > 1 {
            (cur_slot as f32 - (total_pair - 1) as f32 * 0.5) * 32.0
        } else if (n_from.layer as isize - n_to.layer as isize).abs() > 1 {
            compute_skip_curve_offset(&graph, &layer_nodes, edge, nx, ny, label_dims)
        } else {
            0.0
        };

        let ctrl_x = (n_from.cx + n_to.cx) * 0.5 + nx * curve_offset;
        let ctrl_y = (n_from.cy + n_to.cy) * 0.5 + ny * curve_offset;
        let (sx, sy) = boundary_point_on_node(n_from, ctrl_x, ctrl_y);
        let (ex, ey) = boundary_point_on_node(n_to, ctrl_x, ctrl_y);

        let (mid_x, mid_y, tangent_from) = if curve_offset.abs() > 1.0 {
            let steps = 16usize;
            let mut pts = Vec::with_capacity(steps + 1);
            for s in 0..=steps {
                let t = s as f32 / steps as f32;
                let u = 1.0 - t;
                let px = u * u * sx + 2.0 * u * t * ctrl_x + t * t * ex;
                let py = u * u * sy + 2.0 * u * t * ctrl_y + t * t * ey;
                pts.push((px, py));
            }
            let prev = pts[pts.len() - 2];
            let mid = pts[steps / 2];
            items.push(SceneItem::Polyline {
                pts,
                stroke: pal.edge_color,
                width: stroke_w,
                style: edge.style,
            });
            (mid.0, mid.1, prev)
        } else {
            items.push(SceneItem::Line {
                x0: sx,
                y0: sy,
                x1: ex,
                y1: ey,
                stroke: pal.edge_color,
                width: stroke_w,
                style: edge.style,
            });
            let edge_len = ((ex - sx).powi(2) + (ey - sy).powi(2)).sqrt().max(1.0);
            let s_pad = marker_extent(edge.start_marker);
            let e_pad = marker_extent(edge.end_marker);
            let t_mid = (0.5 + (s_pad - e_pad) / (2.0 * edge_len)).clamp(0.35, 0.65);
            (
                sx + (ex - sx) * t_mid,
                sy + (ey - sy) * t_mid,
                (sx, sy),
            )
        };

        emit_endpoint_decoration(
            &mut items,
            edge.end_marker,
            ex,
            ey,
            tangent_from.0,
            tangent_from.1,
            pal.edge_color,
            pal.bg,
        );
        emit_endpoint_decoration(
            &mut items,
            edge.start_marker,
            sx,
            sy,
            ctrl_x,
            ctrl_y,
            pal.edge_color,
            pal.bg,
        );

        if !edge.label.is_empty() {
            let (lw, lh) = label_dims;
            items.push(SceneItem::RoundedRect {
                x0: mid_x - lw * 0.5 - 5.0,
                y0: mid_y - lh * 0.5 - 2.5,
                x1: mid_x + lw * 0.5 + 5.0,
                y1: mid_y + lh * 0.5 + 2.5,
                radius: 4.0,
                fill: Some(pal.pill_bg),
                stroke: Some((pal.subgraph_stroke, 0.8)),
                dashed: false,
            });
            items.push(SceneItem::Text {
                x: mid_x,
                y: mid_y,
                text: edge.label.clone(),
                size: 11.0,
                bold: false,
                italic: false,
                color: pal.text_primary,
                align: SceneTextAlign::Center,
            });
        }
        if !edge.start_label.is_empty() {
            items.push(SceneItem::Text {
                x: sx + (ex - sx) * 0.14 + nx * 10.0,
                y: sy + (ey - sy) * 0.14 + ny * 10.0,
                text: edge.start_label.clone(),
                size: 10.0,
                bold: false,
                italic: false,
                color: pal.text_muted,
                align: SceneTextAlign::Center,
            });
        }
        if !edge.end_label.is_empty() {
            items.push(SceneItem::Text {
                x: ex - (ex - sx) * 0.14 + nx * 10.0,
                y: ey - (ey - sy) * 0.14 + ny * 10.0,
                text: edge.end_label.clone(),
                size: 10.0,
                bold: false,
                italic: false,
                color: pal.text_muted,
                align: SceneTextAlign::Center,
            });
        }
    }

    // 3. Nodes
    for node in &graph.nodes {
        let fill = node.fill_override.unwrap_or(pal.node_fill);
        let stroke = node.stroke_override.unwrap_or(pal.node_stroke);
        let text_col = node.text_override.unwrap_or(pal.text_primary);
        let x0 = node.cx - node.w * 0.5;
        let y0 = node.cy - node.h * 0.5;
        let x1 = node.cx + node.w * 0.5;
        let y1 = node.cy + node.h * 0.5;

        match node.shape {
            FlowShape::Rectangle => {
                items.push(SceneItem::RoundedRect {
                    x0,
                    y0,
                    x1,
                    y1,
                    radius: 3.0,
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                    dashed: false,
                });
            }
            FlowShape::Rounded => {
                items.push(SceneItem::RoundedRect {
                    x0,
                    y0,
                    x1,
                    y1,
                    radius: 10.0,
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                    dashed: false,
                });
            }
            FlowShape::Stadium => {
                items.push(SceneItem::RoundedRect {
                    x0,
                    y0,
                    x1,
                    y1,
                    radius: node.h * 0.5,
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                    dashed: false,
                });
            }
            FlowShape::Subroutine => {
                items.push(SceneItem::RoundedRect {
                    x0,
                    y0,
                    x1,
                    y1,
                    radius: 3.0,
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                    dashed: false,
                });
                items.push(SceneItem::Line {
                    x0: x0 + 7.0,
                    y0,
                    x1: x0 + 7.0,
                    y1,
                    stroke,
                    width: 1.3,
                    style: EdgeLineStyle::Solid,
                });
                items.push(SceneItem::Line {
                    x0: x1 - 7.0,
                    y0,
                    x1: x1 - 7.0,
                    y1,
                    stroke,
                    width: 1.3,
                    style: EdgeLineStyle::Solid,
                });
            }
            FlowShape::Cylinder => {
                items.push(SceneItem::Cylinder {
                    x0,
                    y0,
                    x1,
                    y1,
                    fill,
                    stroke: (stroke, 1.6),
                });
            }
            FlowShape::Circle => {
                items.push(SceneItem::Circle {
                    cx: node.cx,
                    cy: node.cy,
                    r: node.w * 0.5,
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                });
            }
            FlowShape::DoubleCircle => {
                let r = node.w * 0.5;
                items.push(SceneItem::Circle {
                    cx: node.cx,
                    cy: node.cy,
                    r,
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                });
                items.push(SceneItem::Circle {
                    cx: node.cx,
                    cy: node.cy,
                    r: (r - 4.5).max(4.0),
                    fill: None,
                    stroke: Some((stroke, 1.3)),
                });
            }
            FlowShape::Diamond => {
                items.push(SceneItem::Polygon {
                    pts: vec![
                        (node.cx, y0),
                        (x1, node.cy),
                        (node.cx, y1),
                        (x0, node.cy),
                    ],
                    fill: Some(pal.node_alt_fill),
                    stroke: Some((stroke, 1.6)),
                });
            }
            FlowShape::Hexagon => {
                let dx_h = 13.0_f32.min(node.w * 0.25);
                items.push(SceneItem::Polygon {
                    pts: vec![
                        (x0 + dx_h, y0),
                        (x1 - dx_h, y0),
                        (x1, node.cy),
                        (x1 - dx_h, y1),
                        (x0 + dx_h, y1),
                        (x0, node.cy),
                    ],
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                });
            }
            FlowShape::Parallelogram => {
                let sk = 12.0;
                items.push(SceneItem::Polygon {
                    pts: vec![
                        (x0 + sk, y0),
                        (x1, y0),
                        (x1 - sk, y1),
                        (x0, y1),
                    ],
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                });
            }
            FlowShape::ParallelogramAlt => {
                let sk = 12.0;
                items.push(SceneItem::Polygon {
                    pts: vec![
                        (x0, y0),
                        (x1 - sk, y0),
                        (x1, y1),
                        (x0 + sk, y1),
                    ],
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                });
            }
            FlowShape::Trapezoid => {
                let sk = 12.0;
                items.push(SceneItem::Polygon {
                    pts: vec![
                        (x0 + sk, y0),
                        (x1 - sk, y0),
                        (x1, y1),
                        (x0, y1),
                    ],
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                });
            }
            FlowShape::TrapezoidAlt => {
                let sk = 12.0;
                items.push(SceneItem::Polygon {
                    pts: vec![
                        (x0, y0),
                        (x1, y0),
                        (x1 - sk, y1),
                        (x0 + sk, y1),
                    ],
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                });
            }
            FlowShape::Asymmetric => {
                let notch = 11.0;
                items.push(SceneItem::Polygon {
                    pts: vec![
                        (x0, y0),
                        (x1, y0),
                        (x1, y1),
                        (x0, y1),
                        (x0 + notch, node.cy),
                    ],
                    fill: Some(fill),
                    stroke: Some((stroke, 1.6)),
                });
            }
            FlowShape::StateStart => {
                items.push(SceneItem::Circle {
                    cx: node.cx,
                    cy: node.cy,
                    r: 10.0,
                    fill: Some(pal.text_primary),
                    stroke: None,
                });
                continue;
            }
            FlowShape::StateEnd => {
                items.push(SceneItem::Circle {
                    cx: node.cx,
                    cy: node.cy,
                    r: 12.0,
                    fill: Some(pal.bg),
                    stroke: Some((pal.text_primary, 1.8)),
                });
                items.push(SceneItem::Circle {
                    cx: node.cx,
                    cy: node.cy,
                    r: 7.0,
                    fill: Some(pal.text_primary),
                    stroke: None,
                });
                continue;
            }
            FlowShape::ForkBar => {
                items.push(SceneItem::RoundedRect {
                    x0,
                    y0,
                    x1,
                    y1,
                    radius: 3.0,
                    fill: Some(pal.text_primary),
                    stroke: None,
                    dashed: false,
                });
                continue;
            }
            FlowShape::ClassBox => {
                if let Some(ref cb) = node.class_box {
                    let lay = compute_class_box_layout(cb, measurer);
                    items.push(SceneItem::RoundedRect {
                        x0,
                        y0,
                        x1,
                        y1,
                        radius: 4.0,
                        fill: Some(fill),
                        stroke: Some((stroke, 1.6)),
                        dashed: false,
                    });
                    items.push(SceneItem::RoundedRect {
                        x0,
                        y0,
                        x1,
                        y1: y0 + lay.head_h,
                        radius: 4.0,
                        fill: Some(pal.header_fill),
                        stroke: Some((stroke, 1.4)),
                        dashed: false,
                    });
                    if let Some(ref st) = cb.stereotype {
                        let st_y = y0 + 6.0 + lay.stereotype_h * 0.5;
                        items.push(SceneItem::Text {
                            x: node.cx,
                            y: st_y,
                            text: st.clone(),
                            size: 10.0,
                            bold: false,
                            italic: true,
                            color: pal.text_muted,
                            align: SceneTextAlign::Center,
                        });
                        let title_y = y0 + 6.0 + lay.stereotype_h + (lay.head_h - 6.0 - lay.stereotype_h) * 0.5;
                        items.push(SceneItem::Text {
                            x: node.cx,
                            y: title_y,
                            text: cb.title.clone(),
                            size: 13.0,
                            bold: true,
                            italic: false,
                            color: text_col,
                            align: SceneTextAlign::Center,
                        });
                    } else {
                        items.push(SceneItem::Text {
                            x: node.cx,
                            y: y0 + lay.head_h * 0.5,
                            text: cb.title.clone(),
                            size: 13.0,
                            bold: true,
                            italic: false,
                            color: text_col,
                            align: SceneTextAlign::Center,
                        });
                    }
                    let mut cur_y = y0 + lay.head_h + 8.0;
                    for (a, &ah) in cb.attributes.iter().zip(lay.attr_heights.iter()) {
                        items.push(SceneItem::Text {
                            x: x0 + 10.0,
                            y: cur_y + ah * 0.5,
                            text: a.clone(),
                            size: 11.5,
                            bold: false,
                            italic: false,
                            color: text_col,
                            align: SceneTextAlign::Left,
                        });
                        cur_y += ah + 4.0;
                    }
                    if let Some(sep_rel) = lay.sep_y_rel {
                        let sep_y = y0 + sep_rel;
                        items.push(SceneItem::Line {
                            x0,
                            y0: sep_y,
                            x1,
                            y1: sep_y,
                            stroke,
                            width: 1.0,
                            style: EdgeLineStyle::Solid,
                        });
                    }
                    if !cb.methods.is_empty() {
                        cur_y = y0 + lay.meth_top_rel;
                        for (m, &mh) in cb.methods.iter().zip(lay.meth_heights.iter()) {
                            items.push(SceneItem::Text {
                                x: x0 + 10.0,
                                y: cur_y + mh * 0.5,
                                text: m.clone(),
                                size: 11.5,
                                bold: false,
                                italic: false,
                                color: text_col,
                                align: SceneTextAlign::Left,
                            });
                            cur_y += mh + 4.0;
                        }
                    }
                }
                continue;
            }
            FlowShape::EntityBox => {
                if let Some(ref eb) = node.entity_box {
                    let lay = compute_entity_box_layout(eb, measurer);
                    items.push(SceneItem::RoundedRect {
                        x0,
                        y0,
                        x1,
                        y1,
                        radius: 4.0,
                        fill: Some(fill),
                        stroke: Some((stroke, 1.6)),
                        dashed: false,
                    });
                    items.push(SceneItem::RoundedRect {
                        x0,
                        y0,
                        x1,
                        y1: y0 + lay.head_h,
                        radius: 4.0,
                        fill: Some(pal.header_fill),
                        stroke: Some((stroke, 1.4)),
                        dashed: false,
                    });
                    items.push(SceneItem::Text {
                        x: node.cx,
                        y: y0 + lay.head_h * 0.5,
                        text: eb.title.clone(),
                        size: 13.0,
                        bold: true,
                        italic: false,
                        color: text_col,
                        align: SceneTextAlign::Center,
                    });
                    let mut cur_y = y0 + lay.head_h + 8.0;
                    for (line, rh) in lay.row_texts {
                        items.push(SceneItem::Text {
                            x: x0 + 10.0,
                            y: cur_y + rh * 0.5,
                            text: line,
                            size: 11.5,
                            bold: false,
                            italic: false,
                            color: text_col,
                            align: SceneTextAlign::Left,
                        });
                        cur_y += rh + 5.0;
                    }
                }
                continue;
            }
        }

        items.push(SceneItem::Text {
            x: node.cx,
            y: node.cy,
            text: node.label.clone(),
            size: 12.5,
            bold: false,
            italic: false,
            color: text_col,
            align: SceneTextAlign::Center,
        });
    }

    let (b_min_x, b_min_y, b_max_x, b_max_y) = scene_items_bounds(&items);
    let pad = 18.0_f32;
    let shift_x = (pad - b_min_x).max(0.0);
    let shift_y = (pad - b_min_y).max(0.0);
    translate_scene_items(&mut items, shift_x, shift_y);
    scene_w = scene_w.max(b_max_x + shift_x + pad);
    scene_h = scene_h.max(b_max_y + shift_y + pad);

    DiagramScene {
        kind_label,
        width: scene_w,
        height: scene_h,
        bg: pal.bg,
        items,
    }
}

// ---------------------------------------------------------------------------
// 1. Flowchart Parser
// ---------------------------------------------------------------------------

fn parse_node_token(raw: &str, graph: &mut GraphModel, active_sg: Option<usize>) -> Option<usize> {
    let mut s = raw.trim();
    if s.is_empty() {
        return None;
    }

    let mut class_name: Option<String> = None;
    if let Some(cls_idx) = s.find(":::") {
        class_name = Some(s[cls_idx + 3..].trim().to_string());
        s = s[..cls_idx].trim();
    }

    let shape_patterns: &[(&str, &str, FlowShape)] = &[
        ("(((", ")))", FlowShape::DoubleCircle),
        ("((", "))", FlowShape::Circle),
        ("([", "])", FlowShape::Stadium),
        ("[[", "]]", FlowShape::Subroutine),
        ("[(", ")]", FlowShape::Cylinder),
        ("{{", "}}", FlowShape::Hexagon),
        ("[/", "/]", FlowShape::Parallelogram),
        ("[\\", "\\]", FlowShape::ParallelogramAlt),
        ("[/", "\\]", FlowShape::Trapezoid),
        ("[\\", "/]", FlowShape::TrapezoidAlt),
        ("[", "]", FlowShape::Rectangle),
        ("(", ")", FlowShape::Rounded),
        ("{", "}", FlowShape::Diamond),
        (">", "]", FlowShape::Asymmetric),
    ];

    let mut id_part = s;
    let mut label_opt: Option<String> = None;
    let mut shape_opt: Option<FlowShape> = None;

    for &(open, close, shp) in shape_patterns {
        if let Some(open_pos) = s.find(open)
            && open_pos > 0 && s.ends_with(close) && open_pos + open.len() <= s.len() - close.len()
            {
                id_part = s[..open_pos].trim();
                label_opt = Some(clean_mermaid_label(&s[open_pos + open.len()..s.len() - close.len()]));
                shape_opt = Some(shp);
                break;
            }
    }

    let id = id_part.trim();
    if id.is_empty() {
        return None;
    }
    let idx = graph.ensure_node(id, active_sg);
    if let Some(lbl) = label_opt
        && !lbl.is_empty() {
            graph.nodes[idx].label = lbl;
        }
    if let Some(shp) = shape_opt {
        graph.nodes[idx].shape = shp;
    }
    if let Some(cls) = class_name
        && let Some(&(f, st, tc)) = graph.class_defs.get(&cls) {
            graph.nodes[idx].fill_override = f;
            graph.nodes[idx].stroke_override = st;
            graph.nodes[idx].text_override = tc;
        }
    Some(idx)
}

#[derive(Debug, Clone)]
struct ParsedFlowEdge {
    style: EdgeLineStyle,
    start_marker: EdgeEndpointKind,
    end_marker: EdgeEndpointKind,
    label: String,
    byte_len: usize,
}

fn try_parse_flow_edge_at(s: &str) -> Option<ParsedFlowEdge> {
    let trim_start_len = s.len() - s.trim_start().len();
    let rest = &s[trim_start_len..];

    // Check bidirectional or single arrows (longest prefix first)
    let ops: &[(&str, EdgeLineStyle, EdgeEndpointKind, EdgeEndpointKind)] = &[
        ("<-->", EdgeLineStyle::Solid, EdgeEndpointKind::Arrow, EdgeEndpointKind::Arrow),
        ("<-.->", EdgeLineStyle::Dotted, EdgeEndpointKind::Arrow, EdgeEndpointKind::Arrow),
        ("<==>", EdgeLineStyle::Thick, EdgeEndpointKind::Arrow, EdgeEndpointKind::Arrow),
        ("o--o", EdgeLineStyle::Solid, EdgeEndpointKind::Circle, EdgeEndpointKind::Circle),
        ("x--x", EdgeLineStyle::Solid, EdgeEndpointKind::Cross, EdgeEndpointKind::Cross),
        ("-.->", EdgeLineStyle::Dotted, EdgeEndpointKind::None, EdgeEndpointKind::Arrow),
        ("-.-", EdgeLineStyle::Dotted, EdgeEndpointKind::None, EdgeEndpointKind::None),
        ("-->", EdgeLineStyle::Solid, EdgeEndpointKind::None, EdgeEndpointKind::Arrow),
        ("---", EdgeLineStyle::Solid, EdgeEndpointKind::None, EdgeEndpointKind::None),
        ("--o", EdgeLineStyle::Solid, EdgeEndpointKind::None, EdgeEndpointKind::Circle),
        ("--x", EdgeLineStyle::Solid, EdgeEndpointKind::None, EdgeEndpointKind::Cross),
        ("==>", EdgeLineStyle::Thick, EdgeEndpointKind::None, EdgeEndpointKind::Arrow),
        ("===", EdgeLineStyle::Thick, EdgeEndpointKind::None, EdgeEndpointKind::None),
    ];

    for &(op, style, sm, em) in ops {
        if let Some(after) = rest.strip_prefix(op) {
            let mut consumed = trim_start_len + op.len();
            let mut label = String::new();
            let after_ws = after.trim_start();
            let ws_skip = after.len() - after_ws.len();
            if let Some(after_bar) = after_ws.strip_prefix('|')
                && let Some(end_bar) = after_bar.find('|') {
                    label = clean_mermaid_label(&after_bar[..end_bar]);
                    consumed += ws_skip + 1 + end_bar + 1;
                }
            return Some(ParsedFlowEdge {
                style,
                start_marker: sm,
                end_marker: em,
                label,
                byte_len: consumed,
            });
        }
    }

    // Inline text edges: `-- label -->`, `-- label ---`, `-. label .->`, `== label ==>`
    if let Some(after_dash) = rest.strip_prefix("-- ") {
        if let Some(end_pos) = after_dash.find(" -->") {
            let lbl = clean_mermaid_label(&after_dash[..end_pos]);
            return Some(ParsedFlowEdge {
                style: EdgeLineStyle::Solid,
                start_marker: EdgeEndpointKind::None,
                end_marker: EdgeEndpointKind::Arrow,
                label: lbl,
                byte_len: trim_start_len + 3 + end_pos + 4,
            });
        }
        if let Some(end_pos) = after_dash.find(" ---") {
            let lbl = clean_mermaid_label(&after_dash[..end_pos]);
            return Some(ParsedFlowEdge {
                style: EdgeLineStyle::Solid,
                start_marker: EdgeEndpointKind::None,
                end_marker: EdgeEndpointKind::None,
                label: lbl,
                byte_len: trim_start_len + 3 + end_pos + 4,
            });
        }
    }
    if let Some(after_dot) = rest.strip_prefix("-. ")
        && let Some(end_pos) = after_dot.find(" .->") {
            let lbl = clean_mermaid_label(&after_dot[..end_pos]);
            return Some(ParsedFlowEdge {
                style: EdgeLineStyle::Dotted,
                start_marker: EdgeEndpointKind::None,
                end_marker: EdgeEndpointKind::Arrow,
                label: lbl,
                byte_len: trim_start_len + 3 + end_pos + 4,
            });
        }
    if let Some(after_eq) = rest.strip_prefix("== ")
        && let Some(end_pos) = after_eq.find(" ==>") {
            let lbl = clean_mermaid_label(&after_eq[..end_pos]);
            return Some(ParsedFlowEdge {
                style: EdgeLineStyle::Thick,
                start_marker: EdgeEndpointKind::None,
                end_marker: EdgeEndpointKind::Arrow,
                label: lbl,
                byte_len: trim_start_len + 3 + end_pos + 4,
            });
        }

    None
}

fn split_flow_statement(line: &str) -> Vec<(String, Option<ParsedFlowEdge>)> {
    let mut parts: Vec<(String, Option<ParsedFlowEdge>)> = Vec::new();
    let bytes = line.as_bytes();
    let mut in_dq = false;
    let mut in_sq = false;
    let mut paren_depth = 0usize;
    let mut brack_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut seg_start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' if !in_sq => in_dq = !in_dq,
            b'\'' if !in_dq => in_sq = !in_sq,
            b'(' if !in_dq && !in_sq => paren_depth += 1,
            b')' if !in_dq && !in_sq => paren_depth = paren_depth.saturating_sub(1),
            b'[' if !in_dq && !in_sq => brack_depth += 1,
            b']' if !in_dq && !in_sq => brack_depth = brack_depth.saturating_sub(1),
            b'{' if !in_dq && !in_sq => brace_depth += 1,
            b'}' if !in_dq && !in_sq => brace_depth = brace_depth.saturating_sub(1),
            _ => {}
        }

        if !in_dq && !in_sq && paren_depth == 0 && brack_depth == 0 && brace_depth == 0
            && let Some(edge) = try_parse_flow_edge_at(&line[i..]) {
                let node_str = line[seg_start..i].trim().to_string();
                parts.push((node_str, Some(edge.clone())));
                i += edge.byte_len;
                seg_start = i;
                continue;
            }
        i += line[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }

    let last = line[seg_start..].trim().to_string();
    if !last.is_empty() || parts.is_empty() {
        parts.push((last, None));
    }
    parts
}

fn split_ampersand_outside_brackets(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_dq = false;
    let mut start = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '"' => in_dq = !in_dq,
            '(' | '[' | '{' if !in_dq => depth += 1,
            ')' | ']' | '}' if !in_dq => depth = depth.saturating_sub(1),
            '&' if !in_dq && depth == 0 => {
                let seg = s[start..i].trim();
                if !seg.is_empty() {
                    out.push(seg);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn parse_style_kv_props(props: &str) -> NodeStyleTriple {
    let mut fill = None;
    let mut stroke = None;
    let mut color = None;
    for part in props.split(',') {
        if let Some((k, v)) = part.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "fill" => fill = parse_css_color(v),
                "stroke" => stroke = parse_css_color(v),
                "color" => color = parse_css_color(v),
                _ => {}
            }
        }
    }
    (fill, stroke, color)
}

fn build_flowchart_scene(
    lines: &[String],
    pal: DiagramPalette,
    measurer: &mut dyn DiagramTextMeasurer,
) -> DiagramScene {
    let first = lines.first().map(|s| s.as_str()).unwrap_or("");
    let dir_token = first.split_whitespace().nth(1).unwrap_or("TD").to_ascii_uppercase();
    let direction = match dir_token.as_str() {
        "LR" => GraphDirection::LeftRight,
        "RL" => GraphDirection::RightLeft,
        "BT" => GraphDirection::BottomUp,
        _ => GraphDirection::TopDown,
    };

    let mut graph = GraphModel::new(direction);
    let mut sg_stack: Vec<usize> = Vec::new();

    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.is_empty() || lower.starts_with("direction ") {
            continue;
        }
        if lower == "end" {
            sg_stack.pop();
            continue;
        }
        if let Some(after_sg) = trimmed.strip_prefix("subgraph ") {
            let title = if let Some(b_open) = after_sg.find('[') {
                if let Some(b_close) = after_sg.rfind(']') {
                    clean_mermaid_label(&after_sg[b_open + 1..b_close])
                } else {
                    clean_mermaid_label(after_sg)
                }
            } else {
                clean_mermaid_label(after_sg)
            };
            let parent = sg_stack.last().copied();
            let sg_id = graph.subgraphs.len();
            graph.subgraphs.push(SubgraphInfo { title, parent });
            sg_stack.push(sg_id);
            continue;
        }
        if let Some(after_style) = trimmed.strip_prefix("style ") {
            let mut parts = after_style.splitn(2, char::is_whitespace);
            if let (Some(id), Some(props)) = (parts.next(), parts.next()) {
                let idx = graph.ensure_node(id.trim(), sg_stack.last().copied());
                let (f, st, tc) = parse_style_kv_props(props);
                if f.is_some() {
                    graph.nodes[idx].fill_override = f;
                }
                if st.is_some() {
                    graph.nodes[idx].stroke_override = st;
                }
                if tc.is_some() {
                    graph.nodes[idx].text_override = tc;
                }
            }
            continue;
        }
        if let Some(after_cd) = trimmed.strip_prefix("classDef ") {
            let mut parts = after_cd.splitn(2, char::is_whitespace);
            if let (Some(cls), Some(props)) = (parts.next(), parts.next()) {
                graph
                    .class_defs
                    .insert(cls.trim().to_string(), parse_style_kv_props(props));
            }
            continue;
        }
        if let Some(after_cls) = trimmed.strip_prefix("class ") {
            let mut parts = after_cls.split_whitespace();
            if let (Some(ids), Some(cls)) = (parts.next(), parts.next())
                && let Some(&(f, st, tc)) = graph.class_defs.get(cls) {
                    for id in ids.split(',') {
                        let idx = graph.ensure_node(id.trim(), sg_stack.last().copied());
                        graph.nodes[idx].fill_override = f.or(graph.nodes[idx].fill_override);
                        graph.nodes[idx].stroke_override = st.or(graph.nodes[idx].stroke_override);
                        graph.nodes[idx].text_override = tc.or(graph.nodes[idx].text_override);
                    }
                }
            continue;
        }
        if lower.starts_with("linkstyle ") || lower.starts_with("click ") {
            continue;
        }

        let active_sg = sg_stack.last().copied();
        let chain = split_flow_statement(trimmed);
        let mut group_indices: Vec<Vec<usize>> = Vec::with_capacity(chain.len());
        for (node_expr, _) in &chain {
            let mut ids = Vec::new();
            for sub in split_ampersand_outside_brackets(node_expr) {
                if let Some(idx) = parse_node_token(sub, &mut graph, active_sg) {
                    ids.push(idx);
                }
            }
            group_indices.push(ids);
        }

        for k in 0..chain.len().saturating_sub(1) {
            if let Some(ref edge_info) = chain[k].1 {
                for &u in &group_indices[k] {
                    for &v in &group_indices[k + 1] {
                        graph.edges.push(GraphEdge {
                            from: u,
                            to: v,
                            label: edge_info.label.clone(),
                            style: edge_info.style,
                            start_marker: edge_info.start_marker,
                            end_marker: edge_info.end_marker,
                            start_label: String::new(),
                            end_label: String::new(),
                        });
                    }
                }
            }
        }
    }

    build_graph_scene("Flowchart", graph, pal, measurer)
}

// ---------------------------------------------------------------------------
// 2. Sequence Diagram Parser & Renderer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SeqParticipant {
    label: String,
    is_actor: bool,
}

#[derive(Debug, Clone)]
struct SeqMessage {
    from: usize,
    to: usize,
    label: String,
    dashed: bool,
    arrow_kind: EdgeEndpointKind,
    activate_to: bool,
    deactivate_from: bool,
}

#[derive(Debug, Clone)]
enum SeqEvent {
    Message(SeqMessage),
    Note {
        pos: &'static str,
        p1: usize,
        p2: usize,
        text: String,
    },
    Activate(usize),
    Deactivate(usize),
    BlockStart {
        kind: String,
        label: String,
    },
    BlockDivider {
        label: String,
    },
    BlockEnd,
}

fn ensure_seq_participant(
    list: &mut Vec<SeqParticipant>,
    map: &mut HashMap<String, usize>,
    id: &str,
    label: Option<&str>,
    is_actor: bool,
) -> usize {
    let key = id.trim();
    if let Some(&idx) = map.get(key) {
        if let Some(l) = label {
            list[idx].label = clean_mermaid_label(l);
        }
        if is_actor {
            list[idx].is_actor = true;
        }
        return idx;
    }
    let idx = list.len();
    list.push(SeqParticipant {
        label: label.map(clean_mermaid_label).unwrap_or_else(|| key.to_string()),
        is_actor,
    });
    map.insert(key.to_string(), idx);
    idx
}

fn build_sequence_scene(
    lines: &[String],
    pal: DiagramPalette,
    measurer: &mut dyn DiagramTextMeasurer,
) -> DiagramScene {
    let mut participants: Vec<SeqParticipant> = Vec::new();
    let mut part_map: HashMap<String, usize> = HashMap::new();
    let mut events: Vec<SeqEvent> = Vec::new();
    let mut autonumber = false;
    let mut title: Option<String> = None;

    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.is_empty() {
            continue;
        }
        if lower == "autonumber" {
            autonumber = true;
            continue;
        }
        if let Some(t) = trimmed.strip_prefix("title ") {
            title = Some(clean_mermaid_label(t));
            continue;
        }
        if let Some(rest) = trimmed
            .strip_prefix("participant ")
            .or_else(|| trimmed.strip_prefix("actor "))
        {
            let is_actor = trimmed.starts_with("actor ");
            if let Some((id_p, disp_p)) = rest.split_once(" as ") {
                ensure_seq_participant(&mut participants, &mut part_map, id_p, Some(disp_p), is_actor);
            } else {
                ensure_seq_participant(&mut participants, &mut part_map, rest, None, is_actor);
            }
            continue;
        }
        if let Some(id) = trimmed.strip_prefix("activate ") {
            let p = ensure_seq_participant(&mut participants, &mut part_map, id, None, false);
            events.push(SeqEvent::Activate(p));
            continue;
        }
        if let Some(id) = trimmed.strip_prefix("deactivate ") {
            let p = ensure_seq_participant(&mut participants, &mut part_map, id, None, false);
            events.push(SeqEvent::Deactivate(p));
            continue;
        }
        if lower == "end" {
            events.push(SeqEvent::BlockEnd);
            continue;
        }
        if let Some(lbl) = trimmed
            .strip_prefix("else ")
            .or_else(|| trimmed.strip_prefix("and "))
        {
            events.push(SeqEvent::BlockDivider {
                label: clean_mermaid_label(lbl),
            });
            continue;
        }
        if lower == "else" {
            events.push(SeqEvent::BlockDivider {
                label: String::new(),
            });
            continue;
        }
        let block_kw = ["loop ", "alt ", "opt ", "par ", "critical ", "break "]
            .iter()
            .find(|&&kw| lower.starts_with(kw));
        if let Some(&kw) = block_kw {
            let kind = kw.trim().to_string();
            let lbl = clean_mermaid_label(&trimmed[kw.len()..]);
            events.push(SeqEvent::BlockStart { kind, label: lbl });
            continue;
        }
        if lower.starts_with("note ") {
            if let Some((target_part, text_part)) = trimmed.split_once(':') {
                let target_lower = target_part.to_ascii_lowercase();
                let (pos, who) = if let Some(idx) = target_lower.find("left of ") {
                    ("left", target_part[idx + 8..].trim())
                } else if let Some(idx) = target_lower.find("right of ") {
                    ("right", target_part[idx + 9..].trim())
                } else if let Some(idx) = target_lower.find("over ") {
                    ("over", target_part[idx + 5..].trim())
                } else {
                    ("over", "")
                };
                let (p1, p2) = if let Some((a, b)) = who.split_once(',') {
                    let i1 = ensure_seq_participant(&mut participants, &mut part_map, a, None, false);
                    let i2 = ensure_seq_participant(&mut participants, &mut part_map, b, None, false);
                    (i1, i2)
                } else if !who.is_empty() {
                    let i1 = ensure_seq_participant(&mut participants, &mut part_map, who, None, false);
                    (i1, i1)
                } else {
                    (0, 0)
                };
                if !participants.is_empty() {
                    events.push(SeqEvent::Note {
                        pos,
                        p1,
                        p2,
                        text: clean_mermaid_label(text_part),
                    });
                }
            }
            continue;
        }

        // Parse sequence message arrows
        let seq_ops: &[(&str, bool, EdgeEndpointKind)] = &[
            ("-->>", true, EdgeEndpointKind::Arrow),
            ("->>", false, EdgeEndpointKind::Arrow),
            ("--x", true, EdgeEndpointKind::Cross),
            ("-x", false, EdgeEndpointKind::Cross),
            ("--)", true, EdgeEndpointKind::Circle),
            ("-)", false, EdgeEndpointKind::Circle),
            ("-->", true, EdgeEndpointKind::Arrow),
            ("->", false, EdgeEndpointKind::Arrow),
        ];
        let mut matched = None;
        for &(op, dashed, ak) in seq_ops {
            if let Some(op_idx) = trimmed.find(op) {
                let lhs = trimmed[..op_idx].trim();
                let rhs_full = &trimmed[op_idx + op.len()..];
                let (rhs, msg) = rhs_full.split_once(':').unwrap_or((rhs_full, ""));
                let mut rhs_clean = rhs.trim();
                let mut activate_to = false;
                let mut deactivate_from = false;
                if let Some(stripped) = rhs_clean.strip_prefix('+') {
                    activate_to = true;
                    rhs_clean = stripped.trim();
                } else if let Some(stripped) = rhs_clean.strip_prefix('-') {
                    deactivate_from = true;
                    rhs_clean = stripped.trim();
                }
                if !lhs.is_empty() && !rhs_clean.is_empty() {
                    let from = ensure_seq_participant(&mut participants, &mut part_map, lhs, None, false);
                    let to = ensure_seq_participant(&mut participants, &mut part_map, rhs_clean, None, false);
                    matched = Some(SeqMessage {
                        from,
                        to,
                        label: clean_mermaid_label(msg),
                        dashed,
                        arrow_kind: ak,
                        activate_to,
                        deactivate_from,
                    });
                }
                break;
            }
        }
        if let Some(m) = matched {
            events.push(SeqEvent::Message(m));
        }
    }

    if participants.is_empty() {
        return DiagramScene {
            kind_label: "Sequence Diagram",
            width: 260.0,
            height: 64.0,
            bg: pal.bg,
            items: vec![SceneItem::Text {
                x: 130.0,
                y: 32.0,
                text: "Empty Sequence Diagram".to_string(),
                size: 12.5,
                bold: false,
                italic: true,
                color: pal.text_muted,
                align: SceneTextAlign::Center,
            }],
        };
    }

    // Measure participant boxes & column spacing
    let mut box_ws = Vec::with_capacity(participants.len());
    for p in &participants {
        let (w, _) = measurer.measure_block(&p.label, 12.5, true);
        box_ws.push((w + 28.0).max(88.0));
    }

    let mut min_col_gap = vec![128.0_f32; participants.len().saturating_sub(1)];
    for ev in &events {
        if let SeqEvent::Message(m) = ev
            && m.from != m.to {
                let lo = m.from.min(m.to);
                let hi = m.from.max(m.to);
                let span = hi - lo;
                let (mw, _) = measurer.measure_block(&m.label, 11.5, false);
                let needed_per_step = (mw + 42.0) / span as f32;
                for gap in &mut min_col_gap[lo..hi] {
                    *gap = gap.max(needed_per_step);
                }
            }
    }

    let mut col_x = Vec::with_capacity(participants.len());
    let left_pad = 36.0_f32;
    let mut cur_x = left_pad + box_ws[0] * 0.5;
    col_x.push(cur_x);
    for i in 1..participants.len() {
        let step = min_col_gap[i - 1].max((box_ws[i - 1] + box_ws[i]) * 0.5 + 24.0);
        cur_x += step;
        col_x.push(cur_x);
    }

    let title_h = if title.is_some() { 28.0 } else { 0.0 };
    let head_y = 20.0 + title_h;
    let head_h = 40.0;
    let mut cur_y = head_y + head_h + 26.0;
    let mut msg_num = 0usize;

    struct SeqBlockFrame {
        top_y: f32,
        kind: String,
        label: String,
        divs: Vec<(f32, String)>,
    }

    let mut activation_items = Vec::new();
    let mut msg_items = Vec::new();
    let mut active_stacks: Vec<Vec<f32>> = vec![Vec::new(); participants.len()];
    let mut block_stack: Vec<SeqBlockFrame> = Vec::new();
    let mut max_right = col_x.last().copied().unwrap_or(100.0) + box_ws.last().copied().unwrap_or(80.0) * 0.5 + 36.0;

    for ev in &events {
        match ev {
            SeqEvent::Activate(p) => active_stacks[*p].push(cur_y - 10.0),
            SeqEvent::Deactivate(p) => {
                if let Some(sy) = active_stacks[*p].pop() {
                    let ax = col_x[*p];
                    activation_items.push(SceneItem::RoundedRect {
                        x0: ax - 5.0,
                        y0: sy,
                        x1: ax + 5.0,
                        y1: cur_y - 4.0,
                        radius: 2.0,
                        fill: Some(pal.header_fill),
                        stroke: Some((pal.node_stroke, 1.2)),
                        dashed: false,
                    });
                }
            }
            SeqEvent::BlockStart { kind, label } => {
                block_stack.push(SeqBlockFrame {
                    top_y: cur_y,
                    kind: kind.clone(),
                    label: label.clone(),
                    divs: Vec::new(),
                });
                cur_y += 28.0;
            }
            SeqEvent::BlockDivider { label } => {
                if let Some(top) = block_stack.last_mut() {
                    top.divs.push((cur_y, label.clone()));
                }
                cur_y += 24.0;
            }
            SeqEvent::BlockEnd => {
                if let Some(SeqBlockFrame {
                    top_y,
                    kind,
                    label,
                    divs,
                }) = block_stack.pop()
                {
                    let bx0 = col_x.first().copied().unwrap_or(40.0) - 42.0;
                    let bx1 = col_x.last().copied().unwrap_or(200.0) + 42.0;
                    let by1 = cur_y + 6.0;
                    msg_items.push(SceneItem::RoundedRect {
                        x0: bx0,
                        y0: top_y,
                        x1: bx1,
                        y1: by1,
                        radius: 4.0,
                        fill: None,
                        stroke: Some((pal.subgraph_stroke, 1.3)),
                        dashed: false,
                    });
                    let tag_w = (kind.len() as f32 * 7.5 + 18.0).max(46.0);
                    msg_items.push(SceneItem::RoundedRect {
                        x0: bx0,
                        y0: top_y,
                        x1: bx0 + tag_w,
                        y1: top_y + 20.0,
                        radius: 3.0,
                        fill: Some(pal.header_fill),
                        stroke: Some((pal.subgraph_stroke, 1.1)),
                        dashed: false,
                    });
                    msg_items.push(SceneItem::Text {
                        x: bx0 + tag_w * 0.5,
                        y: top_y + 10.0,
                        text: kind,
                        size: 10.5,
                        bold: true,
                        italic: false,
                        color: pal.text_primary,
                        align: SceneTextAlign::Center,
                    });
                    if !label.is_empty() {
                        msg_items.push(SceneItem::Text {
                            x: bx0 + tag_w + 10.0,
                            y: top_y + 10.0,
                            text: format!("[{label}]"),
                            size: 11.0,
                            bold: false,
                            italic: true,
                            color: pal.text_muted,
                            align: SceneTextAlign::Left,
                        });
                    }
                    for (dy, dl) in divs {
                        msg_items.push(SceneItem::Line {
                            x0: bx0,
                            y0: dy,
                            x1: bx1,
                            y1: dy,
                            stroke: pal.subgraph_stroke,
                            width: 1.1,
                            style: EdgeLineStyle::Dotted,
                        });
                        if !dl.is_empty() {
                            msg_items.push(SceneItem::Text {
                                x: bx0 + 10.0,
                                y: dy + 12.0,
                                text: format!("[{dl}]"),
                                size: 10.5,
                                bold: false,
                                italic: true,
                                color: pal.text_muted,
                                align: SceneTextAlign::Left,
                            });
                        }
                    }
                    cur_y += 18.0;
                }
            }
            SeqEvent::Note { pos, p1, p2, text } => {
                let (nw, nh) = measurer.measure_block(text, 11.0, false);
                let box_w = (nw + 20.0).max(70.0);
                let box_h = (nh + 14.0).max(28.0);
                let cx = match *pos {
                    "left" => col_x[*p1] - box_w * 0.5 - 16.0,
                    "right" => col_x[*p1] + box_w * 0.5 + 16.0,
                    _ => (col_x[*p1] + col_x[*p2]) * 0.5,
                };
                max_right = max_right.max(cx + box_w * 0.5 + 20.0);
                msg_items.push(SceneItem::RoundedRect {
                    x0: cx - box_w * 0.5,
                    y0: cur_y,
                    x1: cx + box_w * 0.5,
                    y1: cur_y + box_h,
                    radius: 4.0,
                    fill: Some(pal.note_fill),
                    stroke: Some((pal.note_stroke, 1.2)),
                    dashed: false,
                });
                msg_items.push(SceneItem::Text {
                    x: cx,
                    y: cur_y + box_h * 0.5,
                    text: text.clone(),
                    size: 11.0,
                    bold: false,
                    italic: false,
                    color: pal.text_primary,
                    align: SceneTextAlign::Center,
                });
                cur_y += box_h + 14.0;
            }
            SeqEvent::Message(m) => {
                msg_num += 1;
                let display_lbl = if autonumber {
                    format!("{msg_num}. {}", m.label)
                } else {
                    m.label.clone()
                };
                let style = if m.dashed {
                    EdgeLineStyle::Dotted
                } else {
                    EdgeLineStyle::Solid
                };
                let x_from = col_x[m.from];
                let x_to = col_x[m.to];
                if m.from == m.to {
                    let loop_w = 34.0;
                    let loop_h = 22.0;
                    let pts = vec![
                        (x_from, cur_y + 14.0),
                        (x_from + loop_w, cur_y + 14.0),
                        (x_from + loop_w, cur_y + 14.0 + loop_h),
                        (x_from, cur_y + 14.0 + loop_h),
                    ];
                    msg_items.push(SceneItem::Polyline {
                        pts,
                        stroke: pal.edge_color,
                        width: 1.6,
                        style,
                    });
                    emit_endpoint_decoration(
                        &mut msg_items,
                        m.arrow_kind,
                        x_from,
                        cur_y + 14.0 + loop_h,
                        x_from + loop_w,
                        cur_y + 14.0 + loop_h,
                        pal.edge_color,
                        pal.bg,
                    );
                    if !display_lbl.is_empty() {
                        let (lw, _) = measurer.measure_block(&display_lbl, 11.5, false);
                        max_right = max_right.max(x_from + loop_w + lw + 20.0);
                        msg_items.push(SceneItem::Text {
                            x: x_from + loop_w + 8.0,
                            y: cur_y + 14.0 + loop_h * 0.5,
                            text: display_lbl,
                            size: 11.5,
                            bold: false,
                            italic: false,
                            color: pal.text_primary,
                            align: SceneTextAlign::Left,
                        });
                    }
                    cur_y += 52.0;
                } else {
                    let y_line = cur_y + 18.0;
                    msg_items.push(SceneItem::Line {
                        x0: x_from,
                        y0: y_line,
                        x1: x_to,
                        y1: y_line,
                        stroke: pal.edge_color,
                        width: 1.6,
                        style,
                    });
                    emit_endpoint_decoration(
                        &mut msg_items,
                        m.arrow_kind,
                        x_to,
                        y_line,
                        x_from,
                        y_line,
                        pal.edge_color,
                        pal.bg,
                    );
                    if !display_lbl.is_empty() {
                        msg_items.push(SceneItem::Text {
                            x: (x_from + x_to) * 0.5,
                            y: y_line - 11.0,
                            text: display_lbl,
                            size: 11.5,
                            bold: false,
                            italic: false,
                            color: pal.text_primary,
                            align: SceneTextAlign::Center,
                        });
                    }
                    if m.activate_to {
                        active_stacks[m.to].push(y_line);
                    }
                    if m.deactivate_from
                        && let Some(sy) = active_stacks[m.from].pop() {
                            activation_items.push(SceneItem::RoundedRect {
                                x0: x_from - 5.0,
                                y0: sy,
                                x1: x_from + 5.0,
                                y1: y_line,
                                radius: 2.0,
                                fill: Some(pal.header_fill),
                                stroke: Some((pal.node_stroke, 1.2)),
                                dashed: false,
                            });
                        }
                    cur_y += 42.0;
                }
            }
        }
    }

    // Close any open activation bars
    for (p, stack) in active_stacks.into_iter().enumerate() {
        for sy in stack {
            let ax = col_x[p];
            activation_items.push(SceneItem::RoundedRect {
                x0: ax - 5.0,
                y0: sy,
                x1: ax + 5.0,
                y1: cur_y,
                radius: 2.0,
                fill: Some(pal.header_fill),
                stroke: Some((pal.node_stroke, 1.2)),
                dashed: false,
            });
        }
    }

    let foot_y = cur_y + 12.0;
    let total_h = foot_y + head_h + 18.0;
    let mut items = Vec::new();

    if let Some(ref t) = title {
        items.push(SceneItem::Text {
            x: max_right * 0.5,
            y: 16.0,
            text: t.clone(),
            size: 14.0,
            bold: true,
            italic: false,
            color: pal.text_primary,
            align: SceneTextAlign::Center,
        });
    }

    // Lifelines
    for &cx in &col_x {
        items.push(SceneItem::Line {
            x0: cx,
            y0: head_y + head_h,
            x1: cx,
            y1: foot_y,
            stroke: pal.subgraph_stroke,
            width: 1.3,
            style: EdgeLineStyle::Dotted,
        });
    }

    // Activation bars underneath messages & arrowheads
    items.extend(activation_items);
    // Messages, notes & interaction boxes
    items.extend(msg_items);

    // Top & bottom participant headers
    for (i, p) in participants.iter().enumerate() {
        let cx = col_x[i];
        let bw = box_ws[i];
        for by in [head_y, foot_y] {
            if p.is_actor {
                items.push(SceneItem::Circle {
                    cx,
                    cy: by + 8.0,
                    r: 6.0,
                    fill: Some(pal.node_fill),
                    stroke: Some((pal.node_stroke, 1.5)),
                });
                items.push(SceneItem::Line {
                    x0: cx,
                    y0: by + 14.0,
                    x1: cx,
                    y1: by + 24.0,
                    stroke: pal.node_stroke,
                    width: 1.5,
                    style: EdgeLineStyle::Solid,
                });
                items.push(SceneItem::Line {
                    x0: cx - 10.0,
                    y0: by + 18.0,
                    x1: cx + 10.0,
                    y1: by + 18.0,
                    stroke: pal.node_stroke,
                    width: 1.5,
                    style: EdgeLineStyle::Solid,
                });
                items.push(SceneItem::Text {
                    x: cx,
                    y: by + 33.0,
                    text: p.label.clone(),
                    size: 12.0,
                    bold: true,
                    italic: false,
                    color: pal.text_primary,
                    align: SceneTextAlign::Center,
                });
            } else {
                items.push(SceneItem::RoundedRect {
                    x0: cx - bw * 0.5,
                    y0: by,
                    x1: cx + bw * 0.5,
                    y1: by + head_h,
                    radius: 6.0,
                    fill: Some(pal.node_fill),
                    stroke: Some((pal.node_stroke, 1.5)),
                    dashed: false,
                });
                items.push(SceneItem::Text {
                    x: cx,
                    y: by + head_h * 0.5,
                    text: p.label.clone(),
                    size: 12.5,
                    bold: true,
                    italic: false,
                    color: pal.text_primary,
                    align: SceneTextAlign::Center,
                });
            }
        }
    }

    DiagramScene {
        kind_label: "Sequence Diagram",
        width: max_right.max(200.0),
        height: total_h,
        bg: pal.bg,
        items,
    }
}

// ---------------------------------------------------------------------------
// 3. State Diagram Parser
// ---------------------------------------------------------------------------

fn build_state_scene(
    lines: &[String],
    pal: DiagramPalette,
    measurer: &mut dyn DiagramTextMeasurer,
) -> DiagramScene {
    let mut graph = GraphModel::new(GraphDirection::TopDown);
    let mut sg_stack: Vec<usize> = Vec::new();
    let mut start_counter = 0usize;
    let mut end_counter = 0usize;
    let mut in_note_block = false;

    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if in_note_block {
            if lower == "end note" {
                in_note_block = false;
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if let Some(dir) = trimmed.strip_prefix("direction ") {
            graph.direction = match dir.trim().to_ascii_uppercase().as_str() {
                "LR" => GraphDirection::LeftRight,
                "RL" => GraphDirection::RightLeft,
                "BT" => GraphDirection::BottomUp,
                _ => GraphDirection::TopDown,
            };
            continue;
        }
        if lower.starts_with("note ") && !trimmed.contains(':') {
            in_note_block = true;
            continue;
        }
        if lower.starts_with("note ") {
            continue;
        }
        if trimmed == "}" {
            sg_stack.pop();
            continue;
        }
        if lower.starts_with("state ") && trimmed.ends_with('{') {
            let inner = trimmed[6..trimmed.len() - 1].trim();
            let title = if let Some((disp, _)) = inner.split_once(" as ") {
                clean_mermaid_label(disp)
            } else {
                clean_mermaid_label(inner)
            };
            let parent = sg_stack.last().copied();
            let sg_id = graph.subgraphs.len();
            graph.subgraphs.push(SubgraphInfo { title, parent });
            sg_stack.push(sg_id);
            continue;
        }
        if lower.starts_with("state ") {
            let rest = trimmed[6..].trim();
            if rest.contains("<<choice>>") {
                let id = rest.split_whitespace().next().unwrap_or("");
                if !id.is_empty() {
                    let idx = graph.ensure_node(id, sg_stack.last().copied());
                    graph.nodes[idx].shape = FlowShape::Diamond;
                    graph.nodes[idx].label = "?".to_string();
                }
            } else if rest.contains("<<fork>>") || rest.contains("<<join>>") {
                let id = rest.split_whitespace().next().unwrap_or("");
                if !id.is_empty() {
                    let idx = graph.ensure_node(id, sg_stack.last().copied());
                    graph.nodes[idx].shape = FlowShape::ForkBar;
                }
            } else if let Some((disp, alias)) = rest.split_once(" as ") {
                let idx = graph.ensure_node(alias.trim(), sg_stack.last().copied());
                graph.nodes[idx].label = clean_mermaid_label(disp);
                graph.nodes[idx].shape = FlowShape::Rounded;
            }
            continue;
        }

        if let Some(arrow_pos) = trimmed.find("-->") {
            let lhs = trimmed[..arrow_pos].trim();
            let rhs_full = &trimmed[arrow_pos + 3..];
            let (rhs, lbl) = rhs_full.split_once(':').unwrap_or((rhs_full, ""));
            let rhs = rhs.trim();
            if lhs.is_empty() || rhs.is_empty() {
                continue;
            }
            let active_sg = sg_stack.last().copied();
            let from_idx = if lhs == "[*]" {
                start_counter += 1;
                let id = format!("__start_{start_counter}");
                let idx = graph.ensure_node(&id, active_sg);
                graph.nodes[idx].shape = FlowShape::StateStart;
                idx
            } else {
                let idx = graph.ensure_node(lhs, active_sg);
                if graph.nodes[idx].shape == FlowShape::Rectangle {
                    graph.nodes[idx].shape = FlowShape::Rounded;
                }
                idx
            };
            let to_idx = if rhs == "[*]" {
                end_counter += 1;
                let id = format!("__end_{end_counter}");
                let idx = graph.ensure_node(&id, active_sg);
                graph.nodes[idx].shape = FlowShape::StateEnd;
                idx
            } else {
                let idx = graph.ensure_node(rhs, active_sg);
                if graph.nodes[idx].shape == FlowShape::Rectangle {
                    graph.nodes[idx].shape = FlowShape::Rounded;
                }
                idx
            };
            graph.edges.push(GraphEdge {
                from: from_idx,
                to: to_idx,
                label: clean_mermaid_label(lbl),
                style: EdgeLineStyle::Solid,
                start_marker: EdgeEndpointKind::None,
                end_marker: EdgeEndpointKind::Arrow,
                start_label: String::new(),
                end_label: String::new(),
            });
            continue;
        }

        if let Some((id, desc)) = trimmed.split_once(':') {
            let idx = graph.ensure_node(id.trim(), sg_stack.last().copied());
            graph.nodes[idx].shape = FlowShape::Rounded;
            let d = clean_mermaid_label(desc);
            if !d.is_empty() {
                graph.nodes[idx].label = format!("{}\n{}", id.trim(), d);
            }
        }
    }

    build_graph_scene("State Diagram", graph, pal, measurer)
}

// ---------------------------------------------------------------------------
// 4. Class Diagram Parser
// ---------------------------------------------------------------------------

fn ensure_class_node(graph: &mut GraphModel, name: &str) -> usize {
    let idx = graph.ensure_node(name, None);
    graph.nodes[idx].shape = FlowShape::ClassBox;
    if graph.nodes[idx].class_box.is_none() {
        graph.nodes[idx].class_box = Some(ClassBoxData {
            stereotype: None,
            title: name.to_string(),
            attributes: Vec::new(),
            methods: Vec::new(),
        });
    }
    idx
}

fn add_class_member(cb: &mut ClassBoxData, raw: &str) {
    let t = raw.trim();
    if t.is_empty() {
        return;
    }
    if t.starts_with("<<") && t.ends_with(">>") {
        cb.stereotype = Some(t.to_string());
    } else if t.contains('(') {
        cb.methods.push(t.to_string());
    } else {
        cb.attributes.push(t.to_string());
    }
}

fn build_class_scene(
    lines: &[String],
    pal: DiagramPalette,
    measurer: &mut dyn DiagramTextMeasurer,
) -> DiagramScene {
    let mut graph = GraphModel::new(GraphDirection::TopDown);
    let mut cur_class: Option<usize> = None;

    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(idx) = cur_class {
            if trimmed == "}" {
                cur_class = None;
            } else if let Some(ref mut cb) = graph.nodes[idx].class_box {
                add_class_member(cb, trimmed);
            }
            continue;
        }
        if let Some(dir) = trimmed.strip_prefix("direction ") {
            graph.direction = match dir.trim().to_ascii_uppercase().as_str() {
                "LR" => GraphDirection::LeftRight,
                "RL" => GraphDirection::RightLeft,
                "BT" => GraphDirection::BottomUp,
                _ => GraphDirection::TopDown,
            };
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("class ") {
            if let Some(open_b) = rest.find('{') {
                let name = rest[..open_b].trim();
                let idx = ensure_class_node(&mut graph, name);
                let after = rest[open_b + 1..].trim();
                if let Some(close_b) = after.find('}') {
                    for part in after[..close_b].split(';') {
                        if let Some(ref mut cb) = graph.nodes[idx].class_box {
                            add_class_member(cb, part);
                        }
                    }
                } else {
                    cur_class = Some(idx);
                }
            } else {
                let name = rest.split_whitespace().next().unwrap_or(rest).trim();
                ensure_class_node(&mut graph, name);
            }
            continue;
        }

        // Relationship operators (ordered longest first)
        let rels: &[(&str, EdgeLineStyle, EdgeEndpointKind, EdgeEndpointKind)] = &[
            ("<|..", EdgeLineStyle::Dotted, EdgeEndpointKind::HollowTriangle, EdgeEndpointKind::None),
            ("..|>", EdgeLineStyle::Dotted, EdgeEndpointKind::None, EdgeEndpointKind::HollowTriangle),
            ("<|--", EdgeLineStyle::Solid, EdgeEndpointKind::HollowTriangle, EdgeEndpointKind::None),
            ("--|>", EdgeLineStyle::Solid, EdgeEndpointKind::None, EdgeEndpointKind::HollowTriangle),
            ("*--", EdgeLineStyle::Solid, EdgeEndpointKind::FilledDiamond, EdgeEndpointKind::None),
            ("--*", EdgeLineStyle::Solid, EdgeEndpointKind::None, EdgeEndpointKind::FilledDiamond),
            ("o--", EdgeLineStyle::Solid, EdgeEndpointKind::HollowDiamond, EdgeEndpointKind::None),
            ("--o", EdgeLineStyle::Solid, EdgeEndpointKind::None, EdgeEndpointKind::HollowDiamond),
            ("<..", EdgeLineStyle::Dotted, EdgeEndpointKind::Arrow, EdgeEndpointKind::None),
            ("..>", EdgeLineStyle::Dotted, EdgeEndpointKind::None, EdgeEndpointKind::Arrow),
            ("<--", EdgeLineStyle::Solid, EdgeEndpointKind::Arrow, EdgeEndpointKind::None),
            ("-->", EdgeLineStyle::Solid, EdgeEndpointKind::None, EdgeEndpointKind::Arrow),
            ("..", EdgeLineStyle::Dotted, EdgeEndpointKind::None, EdgeEndpointKind::None),
            ("--", EdgeLineStyle::Solid, EdgeEndpointKind::None, EdgeEndpointKind::None),
        ];

        let mut matched_rel = false;
        for &(op, style, sm, em) in rels {
            if let Some(op_idx) = trimmed.find(op) {
                let lhs_raw = trimmed[..op_idx].trim();
                let rhs_full = &trimmed[op_idx + op.len()..];
                let (rhs_raw, lbl) = rhs_full.split_once(':').unwrap_or((rhs_full, ""));
                let (lhs_name, start_mult) = split_quoted_multiplicity(lhs_raw, false);
                let (rhs_name, end_mult) = split_quoted_multiplicity(rhs_raw, true);
                if !lhs_name.is_empty() && !rhs_name.is_empty() {
                    let u = ensure_class_node(&mut graph, lhs_name);
                    let v = ensure_class_node(&mut graph, rhs_name);
                    graph.edges.push(GraphEdge {
                        from: u,
                        to: v,
                        label: clean_mermaid_label(lbl),
                        style,
                        start_marker: sm,
                        end_marker: em,
                        start_label: start_mult,
                        end_label: end_mult,
                    });
                }
                matched_rel = true;
                break;
            }
        }
        if matched_rel {
            continue;
        }

        if let Some((cname, member)) = trimmed.split_once(':') {
            let idx = ensure_class_node(&mut graph, cname.trim());
            if let Some(ref mut cb) = graph.nodes[idx].class_box {
                add_class_member(cb, member);
            }
        }
    }

    build_graph_scene("Class Diagram", graph, pal, measurer)
}

fn split_quoted_multiplicity(raw: &str, mult_first: bool) -> (&str, String) {
    let t = raw.trim();
    if let Some(q1) = t.find('"')
        && let Some(q2) = t[q1 + 1..].find('"') {
            let mult = t[q1 + 1..q1 + 1 + q2].to_string();
            let name = if mult_first {
                t[q1 + 1 + q2 + 1..].trim()
            } else {
                t[..q1].trim()
            };
            return (name, mult);
        }
    (t, String::new())
}

// ---------------------------------------------------------------------------
// 5. ER Diagram Parser
// ---------------------------------------------------------------------------

fn ensure_entity_node(graph: &mut GraphModel, name: &str) -> usize {
    let idx = graph.ensure_node(name, None);
    graph.nodes[idx].shape = FlowShape::EntityBox;
    if graph.nodes[idx].entity_box.is_none() {
        graph.nodes[idx].entity_box = Some(EntityBoxData {
            title: name.to_string(),
            fields: Vec::new(),
        });
    }
    idx
}

fn parse_er_card_left(s: &str) -> EdgeEndpointKind {
    match s {
        "||" | "|" => EdgeEndpointKind::ErOne,
        "|o" | "o|" => EdgeEndpointKind::ErZeroOrOne,
        "}|" | "|{" => EdgeEndpointKind::ErMany,
        "}o" | "o{" => EdgeEndpointKind::ErZeroOrMany,
        _ => EdgeEndpointKind::None,
    }
}

fn parse_er_card_right(s: &str) -> EdgeEndpointKind {
    match s {
        "||" | "|" => EdgeEndpointKind::ErOne,
        "o|" | "|o" => EdgeEndpointKind::ErZeroOrOne,
        "|{" | "}|" => EdgeEndpointKind::ErMany,
        "o{" | "}o" => EdgeEndpointKind::ErZeroOrMany,
        _ => EdgeEndpointKind::None,
    }
}

fn build_er_scene(
    lines: &[String],
    pal: DiagramPalette,
    measurer: &mut dyn DiagramTextMeasurer,
) -> DiagramScene {
    let mut graph = GraphModel::new(GraphDirection::LeftRight);
    let mut cur_entity: Option<usize> = None;

    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(idx) = cur_entity {
            if trimmed == "}" {
                cur_entity = None;
            } else {
                let toks: Vec<&str> = trimmed.split_whitespace().collect();
                if toks.len() >= 2 {
                    let ty = toks[0].to_string();
                    let name = toks[1].to_string();
                    let key = toks
                        .get(2)
                        .filter(|s| !s.starts_with('"'))
                        .map(|s| (*s).to_string())
                        .unwrap_or_default();
                    if let Some(ref mut eb) = graph.nodes[idx].entity_box {
                        eb.fields.push((ty, name, key));
                    }
                }
            }
            continue;
        }

        if let Some(open_b) = trimmed.find('{')
            && trimmed.ends_with('{') && !trimmed.contains("--") && !trimmed.contains("..") {
                let name = trimmed[..open_b].trim();
                let idx = ensure_entity_node(&mut graph, name);
                cur_entity = Some(idx);
                continue;
            }

        // Look for ER connector (-- or ..) surrounded by cardinality tokens
        let conn = trimmed
            .find("--")
            .map(|p| (p, EdgeLineStyle::Solid))
            .or_else(|| trimmed.find("..").map(|p| (p, EdgeLineStyle::Dotted)));
        if let Some((conn_idx, style)) = conn {
            let before = trimmed[..conn_idx].trim_end();
            let after_conn = &trimmed[conn_idx + 2..];
            let (lhs_tok, left_card) = if let Some(sp) = before.rfind(char::is_whitespace) {
                (before[..sp].trim(), before[sp..].trim())
            } else {
                continue;
            };
            let after_trim = after_conn.trim_start();
            let (rhs_part, lbl) = after_trim.split_once(':').unwrap_or((after_trim, ""));
            let mut rhs_toks = rhs_part.split_whitespace();
            let right_card = rhs_toks.next().unwrap_or("");
            let rhs_name = rhs_toks.next().unwrap_or("");
            if !lhs_tok.is_empty() && !rhs_name.is_empty() {
                let u = ensure_entity_node(&mut graph, lhs_tok);
                let v = ensure_entity_node(&mut graph, rhs_name);
                graph.edges.push(GraphEdge {
                    from: u,
                    to: v,
                    label: clean_mermaid_label(lbl),
                    style,
                    start_marker: parse_er_card_left(left_card),
                    end_marker: parse_er_card_right(right_card),
                    start_label: String::new(),
                    end_label: String::new(),
                });
            }
        }
    }

    build_graph_scene("ER Diagram", graph, pal, measurer)
}

// ---------------------------------------------------------------------------
// 6. Pie Chart Parser & Renderer
// ---------------------------------------------------------------------------

fn build_pie_scene(
    lines: &[String],
    pal: DiagramPalette,
    measurer: &mut dyn DiagramTextMeasurer,
) -> DiagramScene {
    let first = lines.first().map(|s| s.as_str()).unwrap_or("");
    let show_data = first.to_ascii_lowercase().contains("showdata");
    let mut title: Option<String> = None;
    let mut slices: Vec<(String, f64)> = Vec::new();

    if let Some(t_idx) = first.to_ascii_lowercase().find("title ") {
        title = Some(clean_mermaid_label(&first[t_idx + 6..]));
    }

    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(t) = trimmed.strip_prefix("title ") {
            title = Some(clean_mermaid_label(t));
            continue;
        }
        if let Some((lbl_part, val_part)) = trimmed.rsplit_once(':')
            && let Ok(val) = val_part.trim().parse::<f64>()
                && val > 0.0 {
                    slices.push((clean_mermaid_label(lbl_part), val));
                }
    }

    if slices.is_empty() {
        return DiagramScene {
            kind_label: "Pie Chart",
            width: 240.0,
            height: 64.0,
            bg: pal.bg,
            items: vec![SceneItem::Text {
                x: 120.0,
                y: 32.0,
                text: "Empty Pie Chart".to_string(),
                size: 12.5,
                bold: false,
                italic: true,
                color: pal.text_muted,
                align: SceneTextAlign::Center,
            }],
        };
    }

    let total: f64 = slices.iter().map(|(_, v)| *v).sum();
    let title_h = if title.is_some() { 30.0 } else { 0.0 };
    let radius = 92.0_f32;
    let cx = 28.0 + radius;
    let cy = 22.0 + title_h + radius;

    let mut max_legend_w = 120.0_f32;
    let legend_rows: Vec<String> = slices
        .iter()
        .map(|(lbl, val)| {
            let pct = (*val / total) * 100.0;
            if show_data {
                format!("{lbl} ({val:.0}, {pct:.1}%)")
            } else {
                format!("{lbl} ({pct:.1}%)")
            }
        })
        .collect();
    for r in &legend_rows {
        let (rw, _) = measurer.measure_block(r, 12.0, false);
        max_legend_w = max_legend_w.max(rw + 24.0);
    }

    let legend_x = cx + radius + 34.0;
    let scene_w = (legend_x + max_legend_w + 20.0).max(340.0);
    let legend_h = slices.len() as f32 * 24.0;
    let scene_h = (cy + radius + 24.0).max(22.0 + title_h + legend_h + 24.0);
    let mut items = Vec::new();

    if let Some(ref t) = title {
        items.push(SceneItem::Text {
            x: scene_w * 0.5,
            y: 18.0,
            text: t.clone(),
            size: 14.5,
            bold: true,
            italic: false,
            color: pal.text_primary,
            align: SceneTextAlign::Center,
        });
    }

    let mut angle = -std::f64::consts::FRAC_PI_2;
    for (idx, (_, val)) in slices.iter().enumerate() {
        let frac = *val / total;
        let sweep = frac * std::f64::consts::TAU;
        let end_angle = angle + sweep;
        let color = pal.pie_colors[idx % pal.pie_colors.len()];

        let steps = ((sweep * 22.0).ceil() as usize).clamp(4, 48);
        let mut pts = Vec::with_capacity(steps + 2);
        pts.push((cx, cy));
        for s in 0..=steps {
            let a = angle + sweep * (s as f64 / steps as f64);
            pts.push((cx + radius * a.cos() as f32, cy + radius * a.sin() as f32));
        }
        items.push(SceneItem::Polygon {
            pts,
            fill: Some(color),
            stroke: Some((pal.bg, 1.8)),
        });

        if frac >= 0.06 {
            let mid_a = (angle + end_angle) * 0.5;
            let lx = cx + radius * 0.62 * mid_a.cos() as f32;
            let ly = cy + radius * 0.62 * mid_a.sin() as f32;
            items.push(SceneItem::Text {
                x: lx,
                y: ly,
                text: format!("{:.0}%", frac * 100.0),
                size: 11.5,
                bold: true,
                italic: false,
                color: [255, 255, 255, 255],
                align: SceneTextAlign::Center,
            });
        }
        angle = end_angle;
    }

    let legend_top = cy - (legend_h * 0.5).min(radius);
    for (idx, row_txt) in legend_rows.into_iter().enumerate() {
        let color = pal.pie_colors[idx % pal.pie_colors.len()];
        let ry = legend_top + idx as f32 * 24.0 + 10.0;
        items.push(SceneItem::RoundedRect {
            x0: legend_x,
            y0: ry - 6.0,
            x1: legend_x + 13.0,
            y1: ry + 6.0,
            radius: 3.0,
            fill: Some(color),
            stroke: None,
            dashed: false,
        });
        items.push(SceneItem::Text {
            x: legend_x + 20.0,
            y: ry,
            text: row_txt,
            size: 12.0,
            bold: false,
            italic: false,
            color: pal.text_primary,
            align: SceneTextAlign::Left,
        });
    }

    DiagramScene {
        kind_label: "Pie Chart",
        width: scene_w,
        height: scene_h,
        bg: pal.bg,
        items,
    }
}

// ---------------------------------------------------------------------------
// Top-Level Mermaid Compiler + UI & PNG Renderers
// ---------------------------------------------------------------------------

fn compile_mermaid_with_measurer(
    code: &str,
    pal: DiagramPalette,
    measurer: &mut dyn DiagramTextMeasurer,
) -> DiagramScene {
    let lines = preprocess_mermaid_lines(code);
    let Some(first) = lines.first() else {
        return DiagramScene {
            kind_label: "Mermaid Diagram",
            width: 240.0,
            height: 60.0,
            bg: pal.bg,
            items: vec![SceneItem::Text {
                x: 120.0,
                y: 30.0,
                text: "(empty mermaid diagram)".to_string(),
                size: 12.5,
                bold: false,
                italic: true,
                color: pal.text_muted,
                align: SceneTextAlign::Center,
            }],
        };
    };

    let head = first
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match head.as_str() {
        "flowchart" | "graph" => build_flowchart_scene(&lines, pal, measurer),
        "sequencediagram" => build_sequence_scene(&lines, pal, measurer),
        "statediagram" | "statediagram-v2" => build_state_scene(&lines, pal, measurer),
        "classdiagram" | "classdiagram-v2" => build_class_scene(&lines, pal, measurer),
        "erdiagram" => build_er_scene(&lines, pal, measurer),
        "pie" => build_pie_scene(&lines, pal, measurer),
        _ => DiagramScene {
            kind_label: "Mermaid Diagram",
            width: 320.0,
            height: 68.0,
            bg: pal.bg,
            items: vec![SceneItem::Text {
                x: 160.0,
                y: 34.0,
                text: format!("Unsupported Mermaid diagram type: {}", first.trim()),
                size: 12.0,
                bold: false,
                italic: true,
                color: pal.text_muted,
                align: SceneTextAlign::Center,
            }],
        },
    }
}

/// Compiles a Mermaid code string into a positioned `DiagramScene` for inspection or export.
pub fn compile_mermaid_scene(code: &str, pal: DiagramPalette) -> DiagramScene {
    let Ok(fonts) = offscreen_fonts_2x().lock() else {
        struct FallbackMeasurer;
        impl DiagramTextMeasurer for FallbackMeasurer {
            fn measure_block(&mut self, text: &str, size: f32, _: bool) -> (f32, f32) {
                let max_chars = text.lines().map(|l| l.chars().count()).max().unwrap_or(1);
                let line_cnt = text.lines().count().max(1);
                (max_chars as f32 * size * 0.56, line_cnt as f32 * size * 1.25)
            }
        }
        let mut m = FallbackMeasurer;
        return compile_mermaid_with_measurer(code, pal, &mut m);
    };
    let mut measurer = OffscreenDiagramMeasurer(&fonts);
    compile_mermaid_with_measurer(code, pal, &mut measurer)
}

fn c32_from_arr(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn hash_mermaid_code(code: &str) -> u64 {
    let mut h = FxHasher::default();
    code.hash(&mut h);
    h.finish()
}

#[derive(Clone)]
struct CachedMermaidScene {
    code: String,
    scene: Arc<DiagramScene>,
    last_tick: u64,
}

#[derive(Clone, Default)]
struct MermaidSceneCache {
    atlas: Weak<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>,
    tick: u64,
    entries: FxHashMap<(u64, ThemeMode, u32), CachedMermaidScene>,
}

impl MermaidSceneCache {
    fn max_entries() -> usize {
        let ram_gb = (crate::document::system_total_memory_bytes() / (1024 * 1024 * 1024)).max(1);
        (ram_gb.saturating_mul(32) as usize).clamp(64, 512)
    }

    fn sync_atlas(&mut self, cur_atlas: &Arc<egui::epaint::mutex::Mutex<egui::epaint::TextureAtlas>>) {
        if self.atlas.as_ptr() != Arc::as_ptr(cur_atlas) {
            self.entries.clear();
            self.atlas = Arc::downgrade(cur_atlas);
        }
    }

    fn get(&mut self, key: (u64, ThemeMode, u32), code: &str) -> Option<Arc<DiagramScene>> {
        self.tick = self.tick.wrapping_add(1);
        let tick = self.tick;
        let entry = self.entries.get_mut(&key)?;
        if entry.code == code {
            entry.last_tick = tick;
            Some(Arc::clone(&entry.scene))
        } else {
            None
        }
    }

    fn insert(&mut self, key: (u64, ThemeMode, u32), code: &str, scene: Arc<DiagramScene>) {
        let cap = Self::max_entries();
        if self.entries.len() >= cap && !self.entries.contains_key(&key) {
            let mut ticks: Vec<u64> = self.entries.values().map(|e| e.last_tick).collect();
            let mid = ticks.len() / 2;
            let (_, cutoff, _) = ticks.select_nth_unstable(mid);
            let cutoff_val = *cutoff;
            self.entries.retain(|_, v| v.last_tick > cutoff_val);
            if self.entries.len() >= cap {
                self.entries.clear();
            }
        }
        self.tick = self.tick.wrapping_add(1);
        self.entries.insert(
            key,
            CachedMermaidScene {
                code: code.to_string(),
                scene,
                last_tick: self.tick,
            },
        );
    }
}

/// Renders a Mermaid diagram inside `egui::Ui` with smooth scaling and vector shapes.
pub fn render_mermaid_in_ui(
    ui: &mut egui::Ui,
    code: &str,
    max_width: f32,
    theme: &WordTheme,
) -> Response {
    let cur_atlas = ui.fonts(|f| f.texture_atlas());
    let ppp_bits = ui.ctx().pixels_per_point().to_bits();
    let cache_key = (hash_mermaid_code(code), theme.mode, ppp_bits);
    let cache_id = Id::new("md_mermaid_scene_cache_v1");
    let cached_scene = ui.ctx().data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<MermaidSceneCache>(cache_id);
        cache.sync_atlas(&cur_atlas);
        cache.get(cache_key, code)
    });
    let scene = if let Some(hit) = cached_scene {
        hit
    } else {
        let pal = DiagramPalette::from_theme(theme);
        let compiled = {
            let mut measurer = EguiDiagramMeasurer(ui.ctx());
            Arc::new(compile_mermaid_with_measurer(code, pal, &mut measurer))
        };
        ui.ctx().data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<MermaidSceneCache>(cache_id);
            cache.insert(cache_key, code, Arc::clone(&compiled));
        });
        compiled
    };

    let avail_w = max_width.max(160.0);
    let fit_scale = if scene.width > avail_w {
        (avail_w / scene.width).clamp(0.55, 1.0)
    } else {
        1.0
    };
    let disp_w = (scene.width * fit_scale).max(160.0);
    let disp_h = (scene.height * fit_scale).max(48.0);
    let x_offset = ((avail_w - disp_w) * 0.5).max(0.0);
    let alloc_w = avail_w.max(disp_w);

    let (outer_rect, resp) = ui.allocate_exact_size(Vec2::new(alloc_w, disp_h), Sense::hover());
    if !ui.is_rect_visible(outer_rect) {
        return resp;
    }

    let origin = Pos2::new(outer_rect.left() + x_offset, outer_rect.top());
    let bg_rect = Rect::from_min_size(origin, Vec2::new(disp_w, disp_h));
    let painter = ui.painter();
    painter.rect_filled(bg_rect, 6.0, c32_from_arr(scene.bg));
    painter.rect_stroke(
        bg_rect,
        6.0,
        Stroke::new(1.0_f32, theme.border_subtle),
        egui::StrokeKind::Inside,
    );

    let pt = |x: f32, y: f32| Pos2::new(origin.x + x * fit_scale, origin.y + y * fit_scale);

    for item in &scene.items {
        match item {
            SceneItem::RoundedRect {
                x0,
                y0,
                x1,
                y1,
                radius,
                fill,
                stroke,
                dashed: _,
            } => {
                let r = Rect::from_min_max(pt(*x0, *y0), pt(*x1, *y1));
                let cr = *radius * fit_scale;
                if let Some(fc) = fill {
                    painter.rect_filled(r, cr, c32_from_arr(*fc));
                }
                if let Some((sc, sw)) = stroke {
                    painter.rect_stroke(
                        r,
                        cr,
                        Stroke::new(*sw * fit_scale, c32_from_arr(*sc)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            SceneItem::Polygon { pts, fill, stroke } => {
                let p2: Vec<Pos2> = pts.iter().map(|&(x, y)| pt(x, y)).collect();
                let st = stroke
                    .map(|(sc, sw)| Stroke::new(sw * fit_scale, c32_from_arr(sc)))
                    .unwrap_or(Stroke::NONE);
                let fc = fill.map(c32_from_arr).unwrap_or(Color32::TRANSPARENT);
                painter.add(egui::Shape::convex_polygon(p2, fc, st));
            }
            SceneItem::Circle {
                cx,
                cy,
                r,
                fill,
                stroke,
            } => {
                let center = pt(*cx, *cy);
                let sr = *r * fit_scale;
                if let Some(fc) = fill {
                    painter.circle_filled(center, sr, c32_from_arr(*fc));
                }
                if let Some((sc, sw)) = stroke {
                    painter.circle_stroke(center, sr, Stroke::new(*sw * fit_scale, c32_from_arr(*sc)));
                }
            }
            SceneItem::Line {
                x0,
                y0,
                x1,
                y1,
                stroke,
                width,
                style,
            } => {
                let p0 = pt(*x0, *y0);
                let p1 = pt(*x1, *y1);
                let st = Stroke::new(*width * fit_scale, c32_from_arr(*stroke));
                if *style == EdgeLineStyle::Dotted {
                    painter.add(egui::Shape::dashed_line(&[p0, p1], st, 5.0 * fit_scale, 4.0 * fit_scale));
                } else {
                    painter.line_segment([p0, p1], st);
                }
            }
            SceneItem::Polyline {
                pts,
                stroke,
                width,
                style,
            } => {
                let p2: Vec<Pos2> = pts.iter().map(|&(x, y)| pt(x, y)).collect();
                let st = Stroke::new(*width * fit_scale, c32_from_arr(*stroke));
                if *style == EdgeLineStyle::Dotted {
                    painter.add(egui::Shape::dashed_line(&p2, st, 5.0 * fit_scale, 4.0 * fit_scale));
                } else {
                    painter.add(egui::Shape::line(p2, st));
                }
            }
            SceneItem::Cylinder {
                x0,
                y0,
                x1,
                y1,
                fill,
                stroke,
            } => {
                let ry = ((y1 - y0) * 0.14).clamp(5.0, 9.0);
                let fc = c32_from_arr(*fill);
                let st = Stroke::new(stroke.1 * fit_scale, c32_from_arr(stroke.0));
                let body = Rect::from_min_max(pt(*x0, *y0 + ry), pt(*x1, *y1 - ry));
                painter.rect_filled(body, 0.0, fc);
                // Bottom cap ellipse
                let mut bot_pts = Vec::with_capacity(20);
                let rx = (*x1 - *x0) * 0.5;
                let cx = (*x0 + *x1) * 0.5;
                for s in 0..=18 {
                    let a = std::f32::consts::PI * (s as f32 / 18.0);
                    bot_pts.push(pt(cx + rx * a.cos(), *y1 - ry + ry * a.sin()));
                }
                painter.add(egui::Shape::convex_polygon(bot_pts.clone(), fc, Stroke::NONE));
                painter.add(egui::Shape::line(bot_pts, st));
                painter.line_segment([pt(*x0, *y0 + ry), pt(*x0, *y1 - ry)], st);
                painter.line_segment([pt(*x1, *y0 + ry), pt(*x1, *y1 - ry)], st);
                let mut top_pts = Vec::with_capacity(28);
                for s in 0..24 {
                    let a = std::f32::consts::TAU * (s as f32 / 24.0);
                    top_pts.push(pt(cx + rx * a.cos(), *y0 + ry + ry * a.sin()));
                }
                painter.add(egui::Shape::convex_polygon(top_pts, fc, st));
            }
            SceneItem::Text {
                x,
                y,
                text,
                size,
                bold,
                italic,
                color,
                align,
            } => {
                let mut job = LayoutJob::default();
                job.wrap.max_width = f32::INFINITY;
                job.halign = match align {
                    SceneTextAlign::Left => egui::Align::Min,
                    SceneTextAlign::Center => egui::Align::Center,
                    SceneTextAlign::Right => egui::Align::Max,
                };
                job.append(
                    text,
                    0.0,
                    TextFormat {
                        font_id: make_font_id(*size * fit_scale, *bold, false),
                        color: c32_from_arr(*color),
                        italics: *italic,
                        ..Default::default()
                    },
                );
                let galley = ui.fonts(|f| f.layout_job(job));
                let target_x = origin.x + *x * fit_scale;
                let target_y = origin.y + *y * fit_scale;
                let gx = match align {
                    SceneTextAlign::Left => target_x - galley.rect.min.x,
                    SceneTextAlign::Center => target_x - galley.rect.center().x,
                    SceneTextAlign::Right => target_x - galley.rect.max.x,
                };
                let gy = target_y - galley.rect.center().y;
                painter.galley(Pos2::new(gx, gy), galley, c32_from_arr(*color));
            }
        }
    }

    resp
}

fn draw_dashed_segment_canvas(
    canvas: &mut RgbaCanvas,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    width: f32,
    color: [u8; 4],
) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt();
    if len <= 1e-3 {
        return;
    }
    let dash = 5.0_f32;
    let gap = 4.0_f32;
    let mut dist = 0.0_f32;
    while dist < len {
        let end = (dist + dash).min(len);
        let t0 = dist / len;
        let t1 = end / len;
        canvas.draw_line(
            x0 + dx * t0,
            y0 + dy * t0,
            x0 + dx * t1,
            y0 + dy * t1,
            width,
            color,
        );
        dist += dash + gap;
    }
}

/// Rasterizes a Mermaid diagram to crisp PNG bytes `(png_bytes, pixel_w, pixel_h)` for DOCX & PDF export.
pub fn render_mermaid_to_png_bytes(code: &str, scale: f32) -> Option<(Vec<u8>, u32, u32)> {
    let pal = DiagramPalette::export_light();
    let Ok(fonts) = offscreen_fonts_2x().lock() else {
        return None;
    };
    let scene = {
        let mut measurer = OffscreenDiagramMeasurer(&fonts);
        compile_mermaid_with_measurer(code, pal, &mut measurer)
    };

    let req_scale = scale.clamp(1.0, 3.0);
    // `offscreen_fonts_2x()` rasterizes glyph texels at 2.0 pixels-per-point.
    let render_scale = 2.0_f32;
    let mut text_galleys: Vec<(f32, f32, std::sync::Arc<egui::Galley>)> = Vec::new();
    for item in &scene.items {
        if let SceneItem::Text {
            x,
            y,
            text,
            size,
            bold,
            italic,
            color,
            align,
        } = item
        {
            let mut job = LayoutJob::default();
            job.wrap.max_width = f32::INFINITY;
            job.halign = match align {
                SceneTextAlign::Left => egui::Align::Min,
                SceneTextAlign::Center => egui::Align::Center,
                SceneTextAlign::Right => egui::Align::Max,
            };
            job.append(
                text,
                0.0,
                TextFormat {
                    font_id: make_font_id(*size, *bold, false),
                    color: c32_from_arr(*color),
                    italics: *italic,
                    ..Default::default()
                },
            );
            let galley = fonts.layout_job(job);
            let gx = match align {
                SceneTextAlign::Left => *x - galley.rect.min.x,
                SceneTextAlign::Center => *x - galley.rect.center().x,
                SceneTextAlign::Right => *x - galley.rect.max.x,
            };
            let gy = *y - galley.rect.center().y;
            text_galleys.push((gx, gy, galley));
        }
    }
    let font_img = fonts.image();
    drop(fonts);

    let mut canvas = RgbaCanvas::new(scene.width, scene.height, render_scale, scene.bg);
    canvas.stroke_rounded_rect(
        0.5,
        0.5,
        scene.width - 0.5,
        scene.height - 0.5,
        6.0,
        1.0,
        pal.subgraph_stroke,
    );

    for item in &scene.items {
        match item {
            SceneItem::RoundedRect {
                x0,
                y0,
                x1,
                y1,
                radius,
                fill,
                stroke,
                ..
            } => {
                if let Some(fc) = fill {
                    canvas.fill_rounded_rect(*x0, *y0, *x1, *y1, *radius, *fc);
                }
                if let Some((sc, sw)) = stroke {
                    canvas.stroke_rounded_rect(*x0, *y0, *x1, *y1, *radius, *sw, *sc);
                }
            }
            SceneItem::Polygon { pts, fill, stroke } => {
                if let Some(fc) = fill {
                    canvas.fill_polygon(pts, *fc);
                }
                if let Some((sc, sw)) = stroke {
                    canvas.draw_polyline(pts, true, *sw, *sc);
                }
            }
            SceneItem::Circle {
                cx,
                cy,
                r,
                fill,
                stroke,
            } => {
                if let Some(fc) = fill {
                    canvas.fill_circle(*cx, *cy, *r, *fc);
                }
                if let Some((sc, sw)) = stroke {
                    canvas.stroke_circle(*cx, *cy, *r, *sw, *sc);
                }
            }
            SceneItem::Line {
                x0,
                y0,
                x1,
                y1,
                stroke,
                width,
                style,
            } => {
                if *style == EdgeLineStyle::Dotted {
                    draw_dashed_segment_canvas(&mut canvas, *x0, *y0, *x1, *y1, *width, *stroke);
                } else {
                    canvas.draw_line(*x0, *y0, *x1, *y1, *width, *stroke);
                }
            }
            SceneItem::Polyline {
                pts,
                stroke,
                width,
                style,
            } => {
                if *style == EdgeLineStyle::Dotted {
                    for w in pts.windows(2) {
                        draw_dashed_segment_canvas(
                            &mut canvas,
                            w[0].0,
                            w[0].1,
                            w[1].0,
                            w[1].1,
                            *width,
                            *stroke,
                        );
                    }
                } else {
                    canvas.draw_polyline(pts, false, *width, *stroke);
                }
            }
            SceneItem::Cylinder {
                x0,
                y0,
                x1,
                y1,
                fill,
                stroke,
            } => {
                let ry = ((y1 - y0) * 0.14).clamp(5.0, 9.0);
                let rx = (*x1 - *x0) * 0.5;
                let cx = (*x0 + *x1) * 0.5;
                canvas.fill_rounded_rect(*x0, *y0 + ry, *x1, *y1 - ry, 0.0, *fill);
                let mut bot_pts = Vec::with_capacity(20);
                for s in 0..=18 {
                    let a = std::f32::consts::PI * (s as f32 / 18.0);
                    bot_pts.push((cx + rx * a.cos(), *y1 - ry + ry * a.sin()));
                }
                canvas.fill_polygon(&bot_pts, *fill);
                canvas.draw_polyline(&bot_pts, false, stroke.1, stroke.0);
                canvas.draw_line(*x0, *y0 + ry, *x0, *y1 - ry, stroke.1, stroke.0);
                canvas.draw_line(*x1, *y0 + ry, *x1, *y1 - ry, stroke.1, stroke.0);
                let mut top_pts = Vec::with_capacity(24);
                for s in 0..24 {
                    let a = std::f32::consts::TAU * (s as f32 / 24.0);
                    top_pts.push((cx + rx * a.cos(), *y0 + ry + ry * a.sin()));
                }
                canvas.fill_polygon(&top_pts, *fill);
                canvas.draw_polyline(&top_pts, true, stroke.1, stroke.0);
            }
            SceneItem::Text { .. } => {}
        }
    }

    for (gx, gy, galley) in &text_galleys {
        canvas.draw_galley(galley, *gx, *gy, &font_img);
    }

    if (req_scale - render_scale).abs() > 0.05 {
        let img = image::RgbaImage::from_raw(canvas.width, canvas.height, canvas.data)?;
        let mut target_w = ((scene.width * req_scale).round() as u32).clamp(1, 8192);
        let mut target_h = ((scene.height * req_scale).round() as u32).clamp(1, 8192);
        let target_px = (target_w as u64).saturating_mul(target_h as u64);
        if target_px > MAX_CANVAS_PIXELS {
            let factor = (MAX_CANVAS_PIXELS as f64 / target_px as f64).sqrt() as f32;
            target_w = ((target_w as f32 * factor).round() as u32).max(1);
            target_h = ((target_h as f32 * factor).round() as u32).max(1);
        }
        let resized =
            image::imageops::resize(&img, target_w, target_h, image::imageops::FilterType::CatmullRom);
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(resized)
            .write_to(&mut out, image::ImageFormat::Png)
            .ok()?;
        return Some((out.into_inner(), target_w, target_h));
    }

    let w = canvas.width;
    let h = canvas.height;
    let png = canvas.into_png_bytes()?;
    Some((png, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_and_rasterizes_all_six_mermaid_diagram_types() {
        for (name, sample) in MERMAID_TEMPLATES {
            let scene = compile_mermaid_scene(sample, DiagramPalette::export_light());
            assert!(
                scene.width >= 160.0 && scene.height >= 48.0,
                "{name} produced invalid size {}x{}",
                scene.width,
                scene.height
            );
            assert!(
                scene.items.len() >= 4,
                "{name} produced too few scene items: {}",
                scene.items.len()
            );
            let (png, w, h) =
                render_mermaid_to_png_bytes(sample, 2.0).unwrap_or_else(|| panic!("{name} png failed"));
            assert!(w >= 160 && h >= 48);
            assert!(png.starts_with(b"\x89PNG"));
        }
    }

    #[test]
    fn flowchart_supports_subgraphs_styles_and_chained_edges() {
        let code = r#"
        flowchart TD
            subgraph Core
                A([Start]) -->|init| B[(Database)]
                B -.-> C{{Check}}
            end
            C ==> D(((Done)))
            style A fill:#dbeafe,stroke:#2563eb
        "#;
        let scene = compile_mermaid_scene(code, DiagramPalette::export_light());
        assert_eq!(scene.kind_label, "Flowchart");
        assert!(scene
            .items
            .iter()
            .any(|it| matches!(it, SceneItem::Cylinder { .. })));
        assert!(scene
            .items
            .iter()
            .any(|it| matches!(it, SceneItem::Text { text, .. } if text == "Core")));
    }

    #[test]
    fn sequence_supports_alt_loop_notes_and_actors() {
        let code = r#"
        sequenceDiagram
            autonumber
            actor Client
            participant Server
            Client->>+Server: Login
            alt Valid
                Server-->>Client: 200 OK
            else Invalid
                Server-->>-Client: 401
            end
            Note right of Server: Audited
        "#;
        let scene = compile_mermaid_scene(code, DiagramPalette::export_light());
        assert_eq!(scene.kind_label, "Sequence Diagram");
        assert!(scene
            .items
            .iter()
            .any(|it| matches!(it, SceneItem::Text { text, .. } if text == "alt")));
        assert!(scene
            .items
            .iter()
            .any(|it| matches!(it, SceneItem::Text { text, .. } if text == "Audited")));
    }

    #[test]
    fn flowchart_skip_edge_clears_intermediate_node_and_stays_in_bounds() {
        let sample = MERMAID_TEMPLATES[0].1; // Flowchart LR with B -->|No| D skipping C[Process]
        let scene = compile_mermaid_scene(sample, DiagramPalette::export_light());
        let process_rect = scene
            .items
            .iter()
            .find_map(|it| match it {
                SceneItem::RoundedRect { x0, y0, x1, y1, .. } if *x1 - *x0 >= 60.0 => {
                    // Find Process box around text "Process"
                    let proc_txt = scene.items.iter().find_map(|t| match t {
                        SceneItem::Text { x, y, text, .. } if text == "Process" => Some((*x, *y)),
                        _ => None,
                    })?;
                    if proc_txt.0 >= *x0 && proc_txt.0 <= *x1 && proc_txt.1 >= *y0 && proc_txt.1 <= *y1 {
                        Some((*x0, *y0, *x1, *y1))
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .expect("Process rect");
        let no_txt = scene
            .items
            .iter()
            .find_map(|it| match it {
                SceneItem::Text { x, y, text, .. } if text == "No" => Some((*x, *y)),
                _ => None,
            })
            .expect("No label");
        assert!(
            no_txt.1 > process_rect.3 + 8.0 || no_txt.1 < process_rect.1 - 8.0,
            "skip-edge 'No' label {no_txt:?} must clear Process rect {process_rect:?}"
        );
        assert!(no_txt.1 + 10.0 < scene.height);
    }

    #[test]
    fn state_back_edge_clears_intermediate_loading_node() {
        let sample = MERMAID_TEMPLATES[2].1; // State diagram with Error --> Idle : retry skipping Loading
        let scene = compile_mermaid_scene(sample, DiagramPalette::export_light());
        let loading_txt = scene
            .items
            .iter()
            .find_map(|it| match it {
                SceneItem::Text { x, y, text, .. } if text == "Loading" => Some((*x, *y)),
                _ => None,
            })
            .expect("Loading text");
        let loading_rect = scene
            .items
            .iter()
            .find_map(|it| match it {
                SceneItem::RoundedRect { x0, y0, x1, y1, .. }
                    if loading_txt.0 >= *x0
                        && loading_txt.0 <= *x1
                        && loading_txt.1 >= *y0
                        && loading_txt.1 <= *y1 =>
                {
                    Some((*x0, *y0, *x1, *y1))
                }
                _ => None,
            })
            .expect("Loading rect");
        let retry_txt = scene
            .items
            .iter()
            .find_map(|it| match it {
                SceneItem::Text { x, y, text, .. } if text == "retry" => Some((*x, *y)),
                _ => None,
            })
            .expect("retry label");
        assert!(
            retry_txt.0 > loading_rect.2 + 8.0 || retry_txt.0 < loading_rect.0 - 8.0,
            "retry label {retry_txt:?} must clear Loading rect {loading_rect:?}"
        );
    }

    #[test]
    fn class_diagram_members_stay_strictly_inside_boxes_and_clear_divider() {
        let sample = MERMAID_TEMPLATES[3].1; // Class diagram Document & MarkdownDoc
        let scene = compile_mermaid_scene(sample, DiagramPalette::export_light());
        let doc_title = scene
            .items
            .iter()
            .find_map(|it| match it {
                SceneItem::Text { x, y, text, .. } if text == "Document" => Some((*x, *y)),
                _ => None,
            })
            .expect("Document title");
        let doc_box = scene
            .items
            .iter()
            .find_map(|it| match it {
                SceneItem::RoundedRect { x0, y0, x1, y1, .. }
                    if doc_title.0 >= *x0
                        && doc_title.0 <= *x1
                        && doc_title.1 >= *y0
                        && doc_title.1 <= *y1
                        && (*y1 - *y0) > 50.0 =>
                {
                    Some((*x0, *y0, *x1, *y1))
                }
                _ => None,
            })
            .expect("Document box");
        let save_txt = scene
            .items
            .iter()
            .find_map(|it| match it {
                SceneItem::Text { x, y, text, .. } if text == "+save()" => Some((*x, *y)),
                _ => None,
            })
            .expect("+save() text");
        assert!(
            save_txt.1 + 10.0 <= doc_box.3,
            "+save() at y={} must stay cleanly inside Document bottom y1={}",
            save_txt.1,
            doc_box.3
        );
    }

    #[test]
    fn er_diagram_label_clears_markers_and_entity_boxes() {
        let sample = MERMAID_TEMPLATES[4].1; // USER ||--o{ DOCUMENT : owns
        let scene = compile_mermaid_scene(sample, DiagramPalette::export_light());
        let owns_txt = scene
            .items
            .iter()
            .find_map(|it| match it {
                SceneItem::Text { x, y, text, .. } if text == "owns" => Some((*x, *y)),
                _ => None,
            })
            .expect("owns text");
        let entity_boxes: Vec<(f32, f32, f32, f32)> = scene
            .items
            .iter()
            .filter_map(|it| match it {
                SceneItem::RoundedRect { x0, y0, x1, y1, .. } if (*y1 - *y0) > 60.0 => {
                    Some((*x0, *y0, *x1, *y1))
                }
                _ => None,
            })
            .collect();
        assert!(entity_boxes.len() >= 2);
        let gap = entity_boxes[1].0 - entity_boxes[0].2;
        assert!(gap >= 100.0, "ER entity gap {gap} must give room for cardinality + label");
        assert!(owns_txt.0 > entity_boxes[0].2 + 24.0 && owns_txt.0 < entity_boxes[1].0 - 24.0);
    }

    #[test]
    fn mermaid_scene_cache_hits_and_evicts_oldest_half_when_full() {
        let mut cache = MermaidSceneCache::default();
        let cap = MermaidSceneCache::max_entries();
        let sample = Arc::new(compile_mermaid_scene(
            MERMAID_TEMPLATES[0].1,
            DiagramPalette::export_light(),
        ));
        for i in 0..cap {
            let code = format!("flowchart LR\nA{i} --> B{i}");
            let key = (hash_mermaid_code(&code), ThemeMode::Light, 1.0_f32.to_bits());
            cache.insert(key, &code, Arc::clone(&sample));
        }
        assert_eq!(cache.entries.len(), cap);

        // Touch entry 0 so it becomes newest and survives eviction
        let code0 = "flowchart LR\nA0 --> B0";
        let key0 = (hash_mermaid_code(code0), ThemeMode::Light, 1.0_f32.to_bits());
        assert!(cache.get(key0, code0).is_some());

        // Inserting one more triggers bounded half-eviction instead of full wipe
        let extra = "flowchart LR\nExtra --> End";
        let extra_key = (hash_mermaid_code(extra), ThemeMode::Light, 1.0_f32.to_bits());
        cache.insert(extra_key, extra, Arc::clone(&sample));
        assert!(cache.entries.len() < cap);
        assert!(cache.entries.len() >= cap / 2);
        assert!(cache.get(key0, code0).is_some());
        assert!(cache.get(extra_key, extra).is_some());
    }
}
