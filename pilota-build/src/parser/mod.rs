use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::ir::File;

pub(crate) mod protobuf;
pub(crate) mod thrift;

pub use thrift::ThriftParser;

pub use self::protobuf::ProtobufParser;

pub struct ParseResult {
    pub files: Vec<Arc<File>>,
}

pub trait Parser {
    fn input<P: AsRef<Path>>(&mut self, path: P);

    fn inputs<P: AsRef<Path>>(&mut self, paths: impl IntoIterator<Item = P>) {
        paths.into_iter().for_each(|p| self.input(p))
    }

    fn include_dirs(&mut self, dirs: Vec<PathBuf>);

    fn parse(self) -> ParseResult;
}
