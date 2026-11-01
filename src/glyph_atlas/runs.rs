//! Shaping por run para ligaduras de fontes de programação (spike).
//!
//! O caminho normal do atlas modela cada célula como um grafema isolado, então
//! `calt`/`liga` nunca veem `->`, `!=` ou `www` juntos. Aqui uma linha é cortada
//! em runs de células ASCII imprimíveis com o mesmo estilo; cada run é moldado
//! uma vez pelo rustybuzz com `calt` e `liga` ligados. Só sai daqui o que
//! difere do desenho por célula: um glifo cujo cluster cobre N colunas (JetBrains
//! Mono, Cascadia) ou um glifo substituído que ocupa uma coluna mas desenha
//! "para trás" sobre as vizinhas (estilo Fira Code: espaçador + glifo largo).
//! Toda célula que não aparece em [`RunGlyph`] segue pelo caminho por célula,
//! então uma fonte sem GSUB produz uma lista vazia e nada muda no desenho.

use rustybuzz::ttf_parser::Tag;
use rustybuzz::Feature;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Glifo moldado que substitui o desenho por célula de `span` colunas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunGlyph {
    pub start_col: usize,
    /// Colunas cobertas pelo cluster (>= 1).
    pub span: usize,
    pub glyph_id: u16,
    /// `bold | italic << 1`, como as faces do atlas.
    pub style: u8,
    /// Deslocamento horizontal do GPOS em unidades da fonte.
    pub x_offset: i32,
}

/// Entrada mínima do shaper: o escalar da célula e o estilo. Células que não
/// devem participar (grafemas compostos, largas, ocultas) entram como espaço.
pub type RowCell = (char, u8);

const ROW_CACHE_CAPACITY: usize = 512;

/// Só sequências ASCII imprimíveis sem espaço formam runs; espaço quebra o run.
fn shapeable(c: char) -> bool {
    matches!(c, '!'..='~')
}

/// Um run só alfanumérico não liga em Fira Code, JetBrains Mono ou Cascadia,
/// fora `www`; pular esses runs poupa o shaping de quase todo texto corrido.
fn may_ligate(text: &str) -> bool {
    text.bytes().any(|b| !b.is_ascii_alphanumeric()) || text.contains("ww")
}

/// Cache de linhas moldadas. A chave é o hash de (escalares, estilos); o valor
/// é compartilhado com o renderizador por `Arc` para que o atlas continue
/// mutável durante o laço de células.
pub struct RowShaper {
    scratch: Vec<RowCell>,
    cache: HashMap<u64, Arc<[RunGlyph]>>,
    empty: Arc<[RunGlyph]>,
    pub(super) shaped_rows: u64,
    pub(super) cached_rows: u64,
}

impl Default for RowShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl RowShaper {
    pub fn new() -> Self {
        Self {
            scratch: Vec::new(),
            cache: HashMap::with_capacity(ROW_CACHE_CAPACITY),
            empty: Arc::from(Vec::new()),
            shaped_rows: 0,
            cached_rows: 0,
        }
    }

    pub fn empty(&self) -> Arc<[RunGlyph]> {
        self.empty.clone()
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Molda uma linha de `cols` células. `face_for` entrega a face rustybuzz
    /// de um estilo; `shape_row` é chamado só quando a linha não está no cache.
    pub fn row<'f>(
        &mut self,
        cols: usize,
        mut cell: impl FnMut(usize) -> RowCell,
        mut face_for: impl FnMut(u8) -> Option<rustybuzz::Face<'f>>,
    ) -> Arc<[RunGlyph]> {
        self.scratch.clear();
        let mut any = false;
        for col in 0..cols {
            let (c, style) = cell(col);
            if shapeable(c) {
                any = true;
                self.scratch.push((c, style));
            } else {
                self.scratch.push((' ', 0));
            }
        }
        if !any {
            return self.empty.clone();
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.scratch.hash(&mut hasher);
        let key = hasher.finish();
        if let Some(hit) = self.cache.get(&key) {
            self.cached_rows += 1;
            return hit.clone();
        }
        self.shaped_rows += 1;
        let mut out = Vec::new();
        shape_row(&self.scratch, &mut out, &mut face_for);
        let shaped: Arc<[RunGlyph]> = if out.is_empty() {
            self.empty.clone()
        } else {
            Arc::from(out)
        };
        if self.cache.len() >= ROW_CACHE_CAPACITY {
            // Geração nova em vez de LRU de verdade: uma tela cabe inteira em
            // 512 linhas, então o custo de reaquecer é um frame.
            self.cache.clear();
        }
        self.cache.insert(key, shaped.clone());
        shaped
    }
}

/// Molda cada run da linha e acrescenta a `out` só as células cuja saída difere
/// do glifo nominal por célula. Runs com advance fora da grade de células,
/// clusters com mais de um glifo ou `.notdef` são descartados inteiros e ficam
/// com o caminho por célula.
pub fn shape_row<'f>(
    cells: &[RowCell],
    out: &mut Vec<RunGlyph>,
    face_for: &mut impl FnMut(u8) -> Option<rustybuzz::Face<'f>>,
) {
    let features = [
        Feature::new(Tag::from_bytes(b"calt"), 1, ..),
        Feature::new(Tag::from_bytes(b"liga"), 1, ..),
        Feature::new(Tag::from_bytes(b"dlig"), 0, ..),
    ];
    let mut faces: [Option<Option<rustybuzz::Face<'f>>>; 4] = [None, None, None, None];
    let mut text = String::new();
    let mut col = 0;
    while col < cells.len() {
        let (c, style) = cells[col];
        if !shapeable(c) {
            col += 1;
            continue;
        }
        let start = col;
        text.clear();
        while col < cells.len() && shapeable(cells[col].0) && cells[col].1 == style {
            text.push(cells[col].0);
            col += 1;
        }
        if text.len() < 2 || !may_ligate(&text) {
            continue;
        }
        // Uma Face rustybuzz por estilo e por linha: montar a Face custa
        // dezenas de microssegundos, mais que moldar um run curto.
        let slot = &mut faces[usize::from(style & 3)];
        if slot.is_none() {
            *slot = Some(face_for(style));
        }
        let Some(Some(face)) = slot.as_ref() else {
            continue;
        };
        shape_run(face, &features, &text, start, style, out);
    }
}

fn shape_run(
    face: &rustybuzz::Face<'_>,
    features: &[Feature],
    text: &str,
    start: usize,
    style: u8,
    out: &mut Vec<RunGlyph>,
) {
    let Some(unit) = face
        .glyph_index(' ')
        .and_then(|id| face.glyph_hor_advance(id))
        .map(i32::from)
        .filter(|advance| *advance > 0)
    else {
        return;
    };
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(rustybuzz::Direction::LeftToRight);
    buffer.set_script(rustybuzz::script::LATIN);
    let shaped = rustybuzz::shape(face, features, buffer);
    let infos = shaped.glyph_infos();
    let positions = shaped.glyph_positions();
    let bytes = text.as_bytes();
    let first = out.len();
    let mut i = 0;
    while i < infos.len() {
        let cluster = infos[i].cluster as usize;
        let mut j = i;
        while j + 1 < infos.len() && infos[j + 1].cluster as usize == cluster {
            j += 1;
        }
        let next = if j + 1 < infos.len() {
            infos[j + 1].cluster as usize
        } else {
            bytes.len()
        };
        if next <= cluster || cluster >= bytes.len() {
            out.truncate(first);
            return;
        }
        let span = next - cluster;
        let advance: i32 = positions[i..=j].iter().map(|p| p.x_advance).sum();
        // Um cluster com mais de um glifo, um glifo fora da grade ou .notdef
        // deixam o run inteiro no caminho por célula.
        if j != i || (advance - span as i32 * unit).abs() > 1 || infos[i].glyph_id == 0 {
            out.truncate(first);
            return;
        }
        let Ok(glyph_id) = u16::try_from(infos[i].glyph_id) else {
            out.truncate(first);
            return;
        };
        let nominal = face.glyph_index(bytes[cluster] as char).map(|id| id.0);
        let x_offset = positions[i].x_offset;
        let plain =
            span == 1 && nominal == Some(glyph_id) && x_offset == 0 && positions[i].y_offset == 0;
        if !plain {
            out.push(RunGlyph {
                start_col: start + cluster,
                span,
                glyph_id,
                style,
                x_offset,
            });
        }
        i = j + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use font_kit::family_name::FamilyName;
    use font_kit::properties::Properties;
    use font_kit::source::SystemSource;
    use std::time::Instant;

    struct FaceData {
        data: Vec<u8>,
        index: u32,
    }

    fn load(family: FamilyName) -> Option<FaceData> {
        let handle = SystemSource::new()
            .select_best_match(&[family], &Properties::new())
            .ok()?;
        match handle {
            font_kit::handle::Handle::Path { path, font_index } => Some(FaceData {
                data: std::fs::read(path).ok()?,
                index: font_index,
            }),
            font_kit::handle::Handle::Memory { bytes, font_index } => Some(FaceData {
                data: bytes.to_vec(),
                index: font_index,
            }),
        }
    }

    fn ligature_font() -> Option<FaceData> {
        [
            "Fira Code",
            "JetBrains Mono",
            "JetBrainsMono Nerd Font Mono",
            "Cascadia Code",
        ]
        .into_iter()
        .find_map(|name| load(FamilyName::Title(name.to_owned())))
    }

    fn row(text: &str) -> Vec<RowCell> {
        text.chars().map(|c| (c, 0)).collect()
    }

    #[test]
    fn menlo_without_gsub_ligatures_yields_no_run_glyphs() {
        let Some(font) =
            load(FamilyName::Title("Menlo".to_owned())).or_else(|| load(FamilyName::Monospace))
        else {
            return;
        };
        let cells = row("if a != b && c -> d { x <= y; www http:// } ==> ... </>");
        let mut out = Vec::new();
        shape_row(&cells, &mut out, &mut |_| {
            rustybuzz::Face::from_slice(&font.data, font.index)
        });
        assert!(
            out.is_empty(),
            "fonte sem ligaduras produziu glifos de run: {out:?}"
        );
    }

    #[test]
    fn ligature_font_merges_arrow_and_not_equal() {
        let Some(font) = ligature_font() else {
            eprintln!("nenhuma fonte com ligaduras instalada; teste ignorado");
            return;
        };
        let cells = row("a -> b != c");
        let mut out = Vec::new();
        shape_row(&cells, &mut out, &mut |_| {
            rustybuzz::Face::from_slice(&font.data, font.index)
        });
        assert!(
            !out.is_empty(),
            "fonte com ligaduras não produziu glifos de run"
        );
        eprintln!("glifos de run para {:?}: {out:?}", "a -> b != c");
        // `->` começa na coluna 2 e `!=` na coluna 7, em qualquer um dos dois
        // estilos de implementação (cluster largo ou espaçador + glifo largo).
        let covers = |col: usize| {
            out.iter()
                .any(|g| g.start_col <= col && col < g.start_col + g.span)
        };
        assert!(covers(2) || covers(3), "{out:?}");
        assert!(covers(7) || covers(8), "{out:?}");
        for glyph in &out {
            assert!(glyph.start_col + glyph.span <= cells.len());
        }
    }

    #[test]
    fn row_cache_returns_the_same_arc_for_identical_rows() {
        let Some(font) = ligature_font().or_else(|| load(FamilyName::Monospace)) else {
            return;
        };
        let mut shaper = RowShaper::new();
        let cells = row("x -> y");
        let first = shaper.row(
            cells.len(),
            |c| cells[c],
            |_| rustybuzz::Face::from_slice(&font.data, font.index),
        );
        let second = shaper.row(
            cells.len(),
            |c| cells[c],
            |_| rustybuzz::Face::from_slice(&font.data, font.index),
        );
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(shaper.shaped_rows, 1);
        assert_eq!(shaper.cached_rows, 1);
    }

    /// Custo de moldar linhas de 80 colunas: frio (toda linha nova), quente
    /// (cache de linha) e, para comparação, o shaping por célula que o atlas já
    /// paga hoje numa falta de cache. Rodar com
    /// `cargo test --release shape_runs_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn shape_runs_cost() {
        let fonts = [
            ("JetBrainsMono Nerd Font Mono", ligature_font()),
            ("Menlo", load(FamilyName::Title("Menlo".to_owned()))),
        ];
        const ROWS: usize = 10_000;
        let sample =
            "fn main() -> Result<(), E> { if a != b && c >= d || e <= f { x => y; } } // www ==";
        let rows: Vec<Vec<RowCell>> = (0..ROWS)
            .map(|i| {
                let shifted: String = sample
                    .chars()
                    .cycle()
                    .skip(i % sample.len())
                    .take(80)
                    .collect();
                row(&shifted)
            })
            .collect();
        for (name, font) in fonts {
            let Some(font) = font else {
                eprintln!("{name}: indisponível");
                continue;
            };
            let face_from = |_: u8| rustybuzz::Face::from_slice(&font.data, font.index);
            let face_once = rustybuzz::Face::from_slice(&font.data, font.index).unwrap();

            let mut out = Vec::new();
            let mut glyphs = 0usize;
            let start = Instant::now();
            for cells in &rows {
                out.clear();
                shape_row(cells, &mut out, &mut { face_from });
                glyphs += out.len();
            }
            let cold = start.elapsed();

            let start = Instant::now();
            for cells in &rows {
                out.clear();
                shape_row(cells, &mut out, &mut |_| Some(face_once.clone()));
            }
            let cold_shared_face = start.elapsed();

            let mut shaper = RowShaper::new();
            let warm_rows = &rows[..256];
            for cells in warm_rows {
                shaper.row(cells.len(), |c| cells[c], face_from);
            }
            let start = Instant::now();
            for _ in 0..ROWS / warm_rows.len() {
                for cells in warm_rows {
                    shaper.row(cells.len(), |c| cells[c], face_from);
                }
            }
            let warm = start.elapsed();

            let start = Instant::now();
            for cells in &rows {
                for (c, _) in cells {
                    let mut buffer = rustybuzz::UnicodeBuffer::new();
                    let mut bytes = [0; 4];
                    buffer.push_str(c.encode_utf8(&mut bytes));
                    buffer.guess_segment_properties();
                    let shaped = rustybuzz::shape(&face_once, &[], buffer);
                    std::hint::black_box(shaped);
                }
            }
            let per_cell = start.elapsed();

            eprintln!(
                "{name}: {ROWS} linhas x 80 colunas\n  run frio (Face por run):   {:>8.0} ns/linha\n  run frio (Face reusada):   {:>8.0} ns/linha\n  run quente (cache linha):  {:>8.0} ns/linha\n  por célula (80 shapes):    {:>8.0} ns/linha\n  glifos de ligadura: {glyphs}",
                cold.as_nanos() as f64 / ROWS as f64,
                cold_shared_face.as_nanos() as f64 / ROWS as f64,
                warm.as_nanos() as f64 / ROWS as f64,
                per_cell.as_nanos() as f64 / ROWS as f64,
            );
        }
    }
}
