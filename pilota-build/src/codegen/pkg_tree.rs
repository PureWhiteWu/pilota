use std::sync::Arc;

use itertools::Itertools;

use crate::{middle::rir::ItemPath, symbol::Symbol};

#[derive(Debug)]
pub struct PkgNode {
    pub path: ItemPath,
    pub children: Arc<[PkgNode]>,
}

fn from_pkgs(base_path: &[Symbol], pkgs: &[ItemPath]) -> Arc<[PkgNode]> {
    let groups = pkgs.iter().group_by(|p| p.first().unwrap());
    let groups = groups.into_iter();

    Arc::from(
        groups
            .into_iter()
            .map(|(k, v)| {
                let path = base_path
                    .iter()
                    .chain(Some(k).into_iter())
                    .cloned()
                    .collect::<Vec<_>>();

                let pkgs = v
                    .filter(|p| p.len() > 1)
                    .map(|p| ItemPath::from(&p[1..]))
                    .collect::<Vec<_>>();

                let children = from_pkgs(&path, &pkgs);
                PkgNode {
                    path: ItemPath::from(path),
                    children,
                }
            })
            .collect::<Vec<_>>(),
    )
}

impl PkgNode {
    pub fn from_pkgs(pkgs: &[ItemPath]) -> Arc<[PkgNode]> {
        from_pkgs(&[], pkgs)
    }

    pub fn ident(&self) -> Symbol {
        self.path.last().unwrap().clone()
    }
}