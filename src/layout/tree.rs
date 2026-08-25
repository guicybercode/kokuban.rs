use super::{PaneId, SplitDirection};

pub enum PaneNode {
    Leaf(PaneId),
    Split {
        direction: SplitDirection,
        left: Box<PaneNode>,
        right: Box<PaneNode>,
        ratio: f32,
    },
}

impl PaneNode {
    /// Replace the leaf with `target_id` by a split, putting target in the left/top
    /// and `new_id` in the right/bottom.
    pub fn split_leaf(
        &mut self,
        target_id: PaneId,
        new_id: PaneId,
        direction: SplitDirection,
    ) -> bool {
        match self {
            PaneNode::Leaf(id) if *id == target_id => {
                let old = PaneNode::Leaf(target_id);
                let new = PaneNode::Leaf(new_id);
                *self = PaneNode::Split {
                    direction,
                    left: Box::new(old),
                    right: Box::new(new),
                    ratio: 0.5,
                };
                true
            }
            PaneNode::Split { left, right, .. } => {
                left.split_leaf(target_id, new_id, direction)
                    || right.split_leaf(target_id, new_id, direction)
            }
            _ => false,
        }
    }

    /// Remove a leaf, returning its sibling (which replaces the parent split).
    /// Returns Some(removed_node_replacement) if found.
    pub fn remove_leaf(&mut self, target_id: PaneId) -> bool {
        match self {
            PaneNode::Leaf(_) => false,
            PaneNode::Split { left, right, .. } => {
                // Check if left child is the target leaf
                if let PaneNode::Leaf(id) = left.as_ref() {
                    if *id == target_id {
                        // Replace self with right
                        let right = std::mem::replace(right.as_mut(), PaneNode::Leaf(0));
                        *self = right;
                        return true;
                    }
                }
                // Check if right child is the target leaf
                if let PaneNode::Leaf(id) = right.as_ref() {
                    if *id == target_id {
                        let left = std::mem::replace(left.as_mut(), PaneNode::Leaf(0));
                        *self = left;
                        return true;
                    }
                }
                // Recurse
                left.remove_leaf(target_id) || right.remove_leaf(target_id)
            }
        }
    }

    /// Adjust the ratio of the split that is the parent of `target_id` in the given direction.
    pub fn adjust_ratio(&mut self, target_id: PaneId, delta: f32) -> bool {
        match self {
            PaneNode::Leaf(_) => false,
            PaneNode::Split {
                left, right, ratio, ..
            } => {
                let left_has = Self::contains(left, target_id);
                let right_has = Self::contains(right, target_id);
                if left_has && !right_has {
                    // target is in left subtree — check if it's a direct child
                    if let PaneNode::Leaf(id) = left.as_ref() {
                        if *id == target_id {
                            *ratio = (*ratio + delta).clamp(0.1, 0.9);
                            return true;
                        }
                    }
                    return left.adjust_ratio(target_id, delta);
                }
                if right_has && !left_has {
                    if let PaneNode::Leaf(id) = right.as_ref() {
                        if *id == target_id {
                            *ratio = (*ratio + delta).clamp(0.1, 0.9);
                            return true;
                        }
                    }
                    return right.adjust_ratio(target_id, delta);
                }
                false
            }
        }
    }

    pub fn contains(node: &PaneNode, id: PaneId) -> bool {
        match node {
            PaneNode::Leaf(leaf_id) => *leaf_id == id,
            PaneNode::Split { left, right, .. } => {
                Self::contains(left, id) || Self::contains(right, id)
            }
        }
    }

    pub fn collect_leaf_ids(&self, out: &mut Vec<PaneId>) {
        match self {
            PaneNode::Leaf(id) => out.push(*id),
            PaneNode::Split { left, right, .. } => {
                left.collect_leaf_ids(out);
                right.collect_leaf_ids(out);
            }
        }
    }

    pub fn is_single_leaf(&self) -> bool {
        matches!(self, PaneNode::Leaf(_))
    }
}
