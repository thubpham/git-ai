use crate::authorship::attribution_tracker::{Attribution, DiffSegment};
use crate::daemon::checkpoint::is_ai_author_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PriorKind {
    Ai,
    KnownHuman,
    Untracked,
    Mixed,
    None,
}

impl PriorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PriorKind::Ai => "ai",
            PriorKind::KnownHuman => "known_human",
            PriorKind::Untracked => "untracked",
            PriorKind::Mixed => "mixed",
            PriorKind::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditType {
    Add,
    Replace,
}

impl EditType {
    pub fn as_str(self) -> &'static str {
        match self {
            EditType::Add => "add",
            EditType::Replace => "replace",
        }
    }
}

// does not contain add because this describes later edit to an existing span only
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchKind {
    Replace,
    Delete,
}

impl TouchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TouchKind::Replace => "replace",
            TouchKind::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanDraft {
    pub pieces: Vec<(usize, usize)>,
    pub prior_kind: PriorKind,
    pub edit_type: EditType,
}

fn owner_kind(author_id: &str) -> PriorKind {
    if is_ai_author_id(author_id) {
        PriorKind::Ai
    } else if author_id == "human" {
        PriorKind::Untracked
    } else {
        PriorKind::KnownHuman
    }
}

fn prior_kind_of(replaced: &[(usize, usize)], attributions: &[Attribution]) -> PriorKind {
    let mut kinds = Vec::new();
    for &(start, end) in replaced {
        let before = kinds.len();
        kinds.extend(
            attributions
                .iter()
                .filter(|attribution| attribution.start < end && attribution.end > start)
                .map(|attribution| owner_kind(&attribution.author_id)),
        );
        if kinds.len() == before {
            kinds.push(PriorKind::Untracked);
        }
    }
    kinds.sort();
    kinds.dedup();
    match kinds.as_slice() {
        [] => PriorKind::None,
        [only] => *only,
        _ => PriorKind::Mixed,
    }
}

pub fn span_draft(segments: &[DiffSegment], old_attributions: &[Attribution]) -> Option<SpanDraft> {
    let written: Vec<&DiffSegment> = segments
        .iter()
        .filter(|segment| segment.changed && segment.new_range.0 < segment.new_range.1)
        .collect();
    if written.is_empty() {
        return None;
    }
    let replaced: Vec<(usize, usize)> = written
        .iter()
        .map(|segment| segment.old_range)
        .filter(|(start, end)| start < end)
        .collect();
    Some(SpanDraft {
        pieces: written.iter().map(|segment| segment.new_range).collect(),
        prior_kind: prior_kind_of(&replaced, old_attributions),
        edit_type: if replaced.is_empty() {
            EditType::Add
        } else {
            EditType::Replace
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remap {
    pub pieces: Vec<(usize, usize)>,
    pub bytes_removed: usize,
    pub touch: Option<TouchKind>,
}

pub fn remap_pieces(pieces: &[(usize, usize)], segments: &[DiffSegment]) -> Remap {
    let mut surviving = Vec::new();
    let mut bytes_removed = 0;
    let mut replaced_with_new_bytes = false;
    for &(start, end) in pieces {
        for segment in segments {
            let (old_start, old_end) = segment.old_range;
            let overlap_start = start.max(old_start);
            let overlap_end = end.min(old_end);
            if overlap_start >= overlap_end {
                continue;
            }
            if segment.changed {
                bytes_removed += overlap_end - overlap_start;
                replaced_with_new_bytes |= segment.new_range.0 < segment.new_range.1;
            } else {
                let new_start = segment.new_range.0 + (overlap_start - old_start);
                surviving.push((new_start, new_start + (overlap_end - overlap_start)));
            }
        }
    }
    let touch = match (bytes_removed, replaced_with_new_bytes) {
        (0, _) => None,
        (_, true) => Some(TouchKind::Replace),
        (_, false) => Some(TouchKind::Delete),
    };
    Remap {
        pieces: surviving,
        bytes_removed,
        touch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::attribution_tracker::AttributionTracker;

    fn segment(old: (usize, usize), new: (usize, usize), changed: bool) -> DiffSegment {
        DiffSegment {
            old_range: old,
            new_range: new,
            changed,
        }
    }

    fn owned(start: usize, end: usize, author_id: &str) -> Attribution {
        Attribution::new(start, end, author_id.to_string(), 0)
    }

    #[test]
    fn a_pure_addition_replaces_nothing() {
        let segments = [segment((0, 0), (0, 10), true)];

        let draft = span_draft(&segments, &[]).unwrap();

        assert_eq!(draft.pieces, vec![(0, 10)]);
        assert_eq!(draft.prior_kind, PriorKind::None);
        assert_eq!(draft.edit_type, EditType::Add);
    }

    #[test]
    fn replacing_known_human_text_records_that_prior() {
        let segments = [segment((0, 8), (0, 12), true)];

        let draft = span_draft(&segments, &[owned(0, 8, "h_abc")]).unwrap();

        assert_eq!(draft.prior_kind, PriorKind::KnownHuman);
        assert_eq!(draft.edit_type, EditType::Replace);
    }

    #[test]
    fn replacing_ai_text_records_that_prior() {
        let segments = [segment((0, 8), (0, 12), true)];

        let draft = span_draft(&segments, &[owned(0, 8, "session::trace")]).unwrap();

        assert_eq!(draft.prior_kind, PriorKind::Ai);
    }

    #[test]
    fn replacing_unattributed_text_is_untracked() {
        let segments = [segment((0, 8), (0, 12), true)];

        assert_eq!(
            span_draft(&segments, &[owned(0, 8, "human")])
                .unwrap()
                .prior_kind,
            PriorKind::Untracked
        );
        assert_eq!(
            span_draft(&segments, &[]).unwrap().prior_kind,
            PriorKind::Untracked
        );
    }

    #[test]
    fn replacing_text_with_several_owners_is_mixed() {
        let segments = [segment((0, 20), (0, 25), true)];
        let attributions = [owned(0, 10, "h_abc"), owned(10, 20, "session::trace")];

        let draft = span_draft(&segments, &attributions).unwrap();

        assert_eq!(draft.prior_kind, PriorKind::Mixed);
    }

    #[test]
    fn only_overlapping_attributions_count() {
        let segments = [segment((10, 20), (10, 15), true)];
        let attributions = [owned(0, 10, "h_abc"), owned(10, 20, "session::trace")];

        let draft = span_draft(&segments, &attributions).unwrap();

        assert_eq!(draft.prior_kind, PriorKind::Ai);
    }

    #[test]
    fn separate_hunks_become_one_span_with_several_pieces() {
        let segments = [
            segment((0, 0), (0, 5), true),
            segment((0, 30), (5, 35), false),
            segment((30, 38), (35, 50), true),
        ];

        let draft = span_draft(&segments, &[owned(30, 38, "h_abc")]).unwrap();

        assert_eq!(draft.pieces, vec![(0, 5), (35, 50)]);
        assert_eq!(draft.prior_kind, PriorKind::KnownHuman);
        assert_eq!(draft.edit_type, EditType::Replace);
    }

    #[test]
    fn a_pure_deletion_or_no_change_makes_no_span() {
        let deletion = [segment((0, 10), (0, 0), true)];
        let unchanged = [segment((0, 10), (0, 10), false)];

        assert_eq!(span_draft(&deletion, &[owned(0, 10, "h_abc")]), None);
        assert_eq!(span_draft(&unchanged, &[]), None);
    }

    #[test]
    fn a_replaced_word_carries_its_trailing_space_through_the_real_diff() {
        let segments = AttributionTracker::new()
            .compute_diff_segments("const x = 1;\n", "const total = 1;\n")
            .unwrap();

        let draft = span_draft(&segments, &[]).unwrap();

        assert_eq!(draft.pieces, vec![(6, 12)]);
        assert_eq!(draft.edit_type, EditType::Replace);
    }

    #[test]
    fn pieces_after_an_insertion_shift_right_without_a_touch() {
        let segments = [
            segment((0, 0), (0, 5), true),
            segment((0, 20), (5, 25), false),
        ];
        let remap = remap_pieces(&[(10, 15)], &segments);
        assert_eq!(remap.pieces, vec![(15, 20)]);
        assert_eq!(remap.bytes_removed, 0);
        assert_eq!(remap.touch, None);
    }

    #[test]
    fn an_insertion_inside_a_piece_splits_it_without_a_touch() {
        let segments = [
            segment((0, 10), (0, 10), false),
            segment((10, 10), (10, 14), true),
            segment((10, 20), (14, 24), false),
        ];
        let remap = remap_pieces(&[(5, 15)], &segments);
        assert_eq!(remap.pieces, vec![(5, 10), (14, 19)]);
        assert_eq!(remap.touch, None);
    }

    #[test]
    fn overwriting_the_middle_of_a_piece_keeps_both_ends_and_records_a_replace() {
        let segments = [
            segment((0, 8), (0, 8), false),
            segment((8, 12), (8, 9), true),
            segment((12, 20), (9, 17), false),
        ];
        let remap = remap_pieces(&[(5, 15)], &segments);
        assert_eq!(remap.pieces, vec![(5, 8), (9, 12)]);
        assert_eq!(remap.bytes_removed, 4);
        assert_eq!(remap.touch, Some(TouchKind::Replace));
    }

    #[test]
    fn deleting_a_whole_piece_leaves_nothing_and_records_a_delete() {
        let segments = [
            segment((0, 5), (0, 5), false),
            segment((5, 15), (5, 5), true),
            segment((15, 20), (5, 10), false),
        ];
        let remap = remap_pieces(&[(5, 15)], &segments);
        assert_eq!(remap.pieces, Vec::<(usize, usize)>::new());
        assert_eq!(remap.bytes_removed, 10);
        assert_eq!(remap.touch, Some(TouchKind::Delete));
    }

    #[test]
    fn an_edit_elsewhere_leaves_a_piece_where_it_was() {
        let segments = [
            segment((0, 10), (0, 10), false),
            segment((10, 14), (10, 12), true),
            segment((14, 30), (12, 28), false),
        ];
        let remap = remap_pieces(&[(2, 6)], &segments);
        assert_eq!(remap.pieces, vec![(2, 6)]);
        assert_eq!(remap.touch, None);
    }
}
