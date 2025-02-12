use std::{ptr::NonNull, sync::Arc};

use fxhash::FxHashMap;

use crate::{
    index::Idx,
    ir,
    ir::visit::Visitor,
    middle::{
        rir::{
            Arg, Const, DefKind, Enum, EnumVariant, Field, FieldKind, File, Item, ItemPath,
            Literal, Message, Method, MethodSource, NewType, Node, NodeKind, Path, Service,
        },
        ty::{self, Ty},
    },
    rir::Mod,
    symbol::{DefId, FileId, Symbol},
    tags::{TagId, Tags},
};

#[derive(Default)]
struct ModuleData {
    resolutions: SymbolTable,
}

#[derive(Clone, Copy)]
enum ModuleId {
    File(FileId),
    Node(DefId),
}

impl ModuleId {
    pub fn expect_def_id(self) -> DefId {
        match self {
            ModuleId::File(_) => panic!("expect def id"),
            ModuleId::Node(did) => did,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Namespace {
    Value,
    Ty,
}

pub struct CollectDef<'a> {
    resolver: &'a mut Resolver,
    parent: Option<ModuleId>,
}

impl<'a> CollectDef<'a> {
    pub fn new(resolver: &'a mut Resolver) -> CollectDef {
        CollectDef {
            resolver,
            parent: None,
        }
    }
}

impl CollectDef<'_> {
    fn def_item(&mut self, item: &ir::Item, ns: Namespace) -> DefId {
        let parent = self.parent.as_ref().unwrap();
        let did = self.resolver.did_counter.inc_one();
        let table = match parent {
            ModuleId::File(file_id) => self.resolver.file_sym_map.entry(*file_id).or_default(),
            ModuleId::Node(def_id) => {
                &mut self
                    .resolver
                    .def_modules
                    .entry(*def_id)
                    .or_default()
                    .resolutions
            }
        };

        let name = item.name();

        tracing::debug!("def {} with DefId({:?})", name, did);

        if match ns {
            Namespace::Value => table.value.insert(name.clone(), did),
            Namespace::Ty => table.ty.insert(name.clone(), did),
        }
        .is_some()
        {
            tracing::error!("{} is already defined", name);
        };

        did
    }
}

impl ir::visit::Visitor for CollectDef<'_> {
    fn visit_file(&mut self, file: Arc<ir::File>) {
        self.parent = Some(ModuleId::File(file.id));
        ir::visit::walk_file(self, file);
        self.parent = None;
    }

    fn visit_item(&mut self, item: Arc<ir::Item>) {
        if let Some(did) = match &item.kind {
            ir::ItemKind::Message(_)
            | ir::ItemKind::Enum(_)
            | ir::ItemKind::Service(_)
            | ir::ItemKind::NewType(_) => Some(self.def_item(&item, Namespace::Ty)),
            ir::ItemKind::Const(_) => Some(self.def_item(&item, Namespace::Value)),
            ir::ItemKind::Mod(_) => Some(self.def_item(&item, Namespace::Ty)),
            ir::ItemKind::Use(_) => None,
        } {
            let prev_parent = self.parent.replace(ModuleId::Node(did));
            ir::visit::walk_item(self, item);
            self.parent = prev_parent;
        }
    }
}

#[derive(Default)]
pub struct SymbolTable {
    pub(crate) value: FxHashMap<Symbol, DefId>,
    pub(crate) ty: FxHashMap<Symbol, DefId>,
}

pub struct Resolver {
    pub(crate) did_counter: DefId,
    pub(crate) file_sym_map: FxHashMap<FileId, SymbolTable>,
    def_modules: FxHashMap<DefId, ModuleData>,
    blocks: Vec<NonNull<SymbolTable>>,

    parent_node: Option<DefId>,
    nodes: FxHashMap<DefId, Node>,
    tags_id_counter: TagId,
    tags: FxHashMap<TagId, Arc<Tags>>,
    cur_file: Option<FileId>,
    ir_files: FxHashMap<FileId, Arc<ir::File>>,
}

impl Default for Resolver {
    fn default() -> Self {
        Resolver {
            tags_id_counter: TagId::from_usize(0),
            tags: Default::default(),
            blocks: Default::default(),
            def_modules: Default::default(),
            did_counter: DefId::from_usize(0),
            file_sym_map: Default::default(),
            nodes: Default::default(),
            ir_files: Default::default(),
            cur_file: None,
            parent_node: None,
        }
    }
}

pub struct ResolveResult {
    pub files: FxHashMap<FileId, Arc<File>>,
    pub nodes: FxHashMap<DefId, Node>,
    pub tags: FxHashMap<TagId, Arc<Tags>>,
}

impl Resolver {
    fn resolve_sym(&self, ns: Namespace, sym: Symbol) -> Option<ModuleId> {
        let def_id = self
            .blocks
            .iter()
            .rev()
            .find_map(|b| {
                let b = unsafe { b.as_ref() };
                match ns {
                    Namespace::Value => b.value.get(&sym),
                    Namespace::Ty => b.ty.get(&sym),
                }
                .or_else(|| {
                    // fuzzy find for protobuf
                    match ns {
                        Namespace::Value => b.value.get(&sym.to_snake_case()).and_then(|def_id| {
                            self.def_modules[def_id].resolutions.value.get(&sym)
                        }),
                        Namespace::Ty => b
                            .ty
                            .get(&sym.to_snake_case())
                            .and_then(|def_id| self.def_modules[def_id].resolutions.ty.get(&sym)),
                    }
                })
            })
            .copied();

        def_id.map(ModuleId::Node).or_else(|| {
            let cur_file = self.ir_files.get(self.cur_file.as_ref().unwrap()).unwrap();
            cur_file.uses.get(&sym).map(|id| ModuleId::File(*id))
        })
    }

    pub fn resolve_files(mut self, files: &[Arc<ir::File>]) -> ResolveResult {
        files.iter().for_each(|f| {
            let mut collect = CollectDef::new(&mut self);
            collect.visit_file(f.clone());
            self.ir_files.insert(f.id, f.clone());
        });

        let files = files
            .iter()
            .map(|f| (f.id, Arc::from(self.lower_file(f))))
            .collect::<FxHashMap<_, _>>();

        ResolveResult {
            tags: self.tags,
            files,
            nodes: self.nodes,
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(name = &**f.name))]
    fn lower_field(&mut self, f: &ir::Field) -> Arc<Field> {
        tracing::info!("lower filed {}, ty: {:?}", f.name, f.ty.kind);
        let did = self.did_counter.inc_one();
        let tag_id = self.tags_id_counter.inc_one();
        self.tags.insert(tag_id, f.tags.clone());
        let f = Arc::from(Field {
            did,
            id: f.id,
            kind: match f.kind {
                ir::FieldKind::Required => FieldKind::Required,
                ir::FieldKind::Optional => FieldKind::Optional,
            },
            name: f.name.to_snake_case(),
            ty: self.lower_type(&f.ty),
        });

        self.nodes
            .insert(did, self.mk_node(NodeKind::Field(f.clone()), tag_id));

        f
    }

    fn mk_node(&self, kind: NodeKind, tags: TagId) -> Node {
        Node {
            tags,
            parent: self.parent_node,
            file_id: self.cur_file.unwrap(),
            kind,
        }
    }

    fn lower_type(&mut self, ty: &ir::Ty) -> Ty {
        let kind = match &ty.kind {
            ir::TyKind::String => ty::String,
            ir::TyKind::Void => ty::Void,
            ir::TyKind::U8 => ty::U8,
            ir::TyKind::Bool => ty::Bool,
            ir::TyKind::Bytes => ty::Bytes,
            ir::TyKind::I8 => ty::I8,
            ir::TyKind::I16 => ty::I16,
            ir::TyKind::I32 => ty::I32,
            ir::TyKind::I64 => ty::I64,
            ir::TyKind::F64 => ty::F64,
            ir::TyKind::Vec(ty) => ty::Vec(Arc::from(self.lower_type(ty))),
            ir::TyKind::Set(ty) => ty::Set(Arc::from(self.lower_type(ty))),
            ir::TyKind::Map(k, v) => {
                ty::Map(Arc::from(self.lower_type(k)), Arc::from(self.lower_type(v)))
            }
            ir::TyKind::Path(p) => ty::Path(self.lower_path(p, Namespace::Ty)),
            ir::TyKind::UInt64 => todo!(),
            ir::TyKind::UInt32 => todo!(),
            ir::TyKind::F32 => todo!(),
        };
        let tags_id = self.tags_id_counter.inc_one();

        self.tags.insert(tags_id, ty.tags.clone());

        Ty { kind, tags_id }
    }

    fn lower_path(&self, p: &ir::Path, ns: Namespace) -> Path {
        let mut module_id = match ns {
            Namespace::Value => &[Namespace::Value, Namespace::Ty] as &[_],
            Namespace::Ty => &[Namespace::Ty],
        }
        .iter()
        .find_map(|ns| self.resolve_sym(*ns, p.segments[0].sym.clone()))
        .unwrap_or_else(|| panic!("undefined ident {}", p.segments[0].sym));

        p.segments[1..].iter().for_each(|ident| {
            module_id = match module_id {
                ModuleId::File(file_id) => {
                    let file = self.ir_files.get(&file_id).unwrap();
                    let table = self.file_sym_map.get(&file_id).unwrap();
                    let table = match ns {
                        Namespace::Value => &table.value,
                        Namespace::Ty => &table.ty,
                    };
                    ModuleId::Node(*table.get(ident).unwrap_or_else(|| {
                        panic!("can not find {} in file {:?}", ident, file.package)
                    }))
                }
                ModuleId::Node(def_id) => match &self.nodes[&def_id].kind {
                    NodeKind::Item(item) => match &**item {
                        Item::Enum(e) => ModuleId::Node(
                            e.variants.iter().find(|v| &v.name == ident).unwrap().did,
                        ),
                        Item::Mod(_) => {
                            let table = match ns {
                                Namespace::Value => &self.def_modules[&def_id].resolutions.value,
                                Namespace::Ty => &self.def_modules[&def_id].resolutions.ty,
                            };

                            ModuleId::Node(
                                *table
                                    .get(ident)
                                    .unwrap_or_else(|| panic!("can not find {}", ident)),
                            )
                        }
                        _ => panic!("invalid item"),
                    },
                    _ => panic!("invalid node"),
                },
            }
        });

        let (kind, did) = match module_id {
            ModuleId::File(_) => panic!(""),
            ModuleId::Node(def_id) => match ns {
                Namespace::Value => (DefKind::Value, def_id),
                Namespace::Ty => (DefKind::Type, def_id),
            },
        };

        Path { kind, did }
    }

    #[tracing::instrument(level = "debug", skip(self, s), fields(name = &**s.name))]
    fn lower_message(&mut self, s: &ir::Message) -> Message {
        Message {
            name: s.name.clone(),
            fields: s.fields.iter().map(|f| self.lower_field(f)).collect(),
        }
    }

    fn lower_enum(&mut self, e: &ir::Enum) -> Enum {
        Enum {
            name: e.name.clone(),
            variants: {
                e.variants
                    .iter()
                    .map(|v| {
                        let tag_id = self.tags_id_counter.inc_one();
                        let did = self.did_counter.inc_one();
                        if !v.tags.is_empty() {
                            self.tags.insert(tag_id, v.tags.clone());
                        }
                        let e = Arc::from(EnumVariant {
                            id: v.id,
                            did,
                            name: v.name.clone(),
                            discr: v.discr,
                            fields: v.fields.iter().map(|p| self.lower_type(p)).collect(),
                        });
                        self.nodes
                            .insert(did, self.mk_node(NodeKind::Variant(e.clone()), tag_id));
                        e
                    })
                    .collect()
            },
            repr: e.repr,
        }
    }

    fn lower_service(&mut self, s: &ir::Service) -> Service {
        Service {
            name: s.name.clone(),
            methods: s
                .methods
                .iter()
                .map(|m| {
                    let def_id = self.did_counter.inc_one();
                    let tags_id = self.tags_id_counter.inc_one();
                    self.tags.insert(tags_id, m.tags.clone());
                    let method = Arc::from(Method {
                        def_id,
                        source: MethodSource::Own,
                        name: m.name.clone(),
                        args: m
                            .args
                            .iter()
                            .map(|a| Arg {
                                ty: self.lower_type(&a.ty),
                                name: a.name.clone(),
                                id: a.id,
                            })
                            .collect(),
                        ret: self.lower_type(&m.ret),
                        oneway: m.oneway,
                        exceptions: m
                            .exceptions
                            .as_ref()
                            .map(|p| self.lower_path(p, Namespace::Ty)),
                    });
                    self.nodes.insert(
                        def_id,
                        self.mk_node(NodeKind::Method(method.clone()), tags_id),
                    );

                    method
                })
                .collect(),
            extend: s
                .extend
                .iter()
                .map(|p| self.lower_path(p, Namespace::Ty))
                .collect(),
        }
    }

    fn lower_type_alias(&mut self, t: &ir::NewType) -> NewType {
        NewType {
            name: t.name.clone(),
            ty: self.lower_type(&t.ty),
        }
    }

    fn lower_lit(&self, l: &ir::Literal) -> Literal {
        match l {
            ir::Literal::Path(p) => Literal::Path(self.lower_path(p, Namespace::Value)),
            ir::Literal::String(s) => Literal::String(s.clone()),
            ir::Literal::Int(i) => Literal::Int(*i),
            ir::Literal::Float(f) => Literal::Float(f.clone()),
            ir::Literal::List(l) => Literal::List(l.iter().map(|l| self.lower_lit(l)).collect()),
            ir::Literal::Map(l) => Literal::Map(
                l.iter()
                    .map(|(k, v)| (self.lower_lit(k), self.lower_lit(v)))
                    .collect(),
            ),
        }
    }

    fn lower_const(&mut self, c: &ir::Const) -> Const {
        Const {
            name: c.name.clone(),
            ty: self.lower_type(&c.ty),
            lit: self.lower_lit(&c.lit),
        }
    }

    fn lower_mod(&mut self, m: &ir::Mod, def_id: DefId) -> Mod {
        self.blocks.push(NonNull::from(
            &self.def_modules.get(&def_id).unwrap().resolutions,
        ));

        let items = m
            .items
            .iter()
            .filter_map(|i| self.lower_item(i))
            .collect::<Vec<_>>();

        self.blocks.pop();

        Mod {
            name: m.name.clone(),
            items,
        }
    }

    fn lower_item(&mut self, item: &ir::Item) -> Option<DefId> {
        if let ir::ItemKind::Use(_) = &item.kind {
            return None;
        }

        let name = item.name();
        let tags = &item.tags;

        let def_id = self
            .resolve_sym(
                match &item.kind {
                    ir::ItemKind::Const(_) => Namespace::Value,
                    _ => Namespace::Ty,
                },
                name.clone(),
            )
            .unwrap_or_else(|| panic!("can not find {}", name))
            .expect_def_id();

        let old_parent = self.parent_node.replace(def_id);

        let item = Arc::new(match &item.kind {
            ir::ItemKind::Message(s) => Item::Message(self.lower_message(s)),
            ir::ItemKind::Enum(e) => Item::Enum(self.lower_enum(e)),
            ir::ItemKind::Service(s) => Item::Service(self.lower_service(s)),
            ir::ItemKind::NewType(t) => Item::NewType(self.lower_type_alias(t)),
            ir::ItemKind::Const(c) => Item::Const(self.lower_const(c)),
            ir::ItemKind::Mod(m) => Item::Mod(self.lower_mod(m, def_id)),
            ir::ItemKind::Use(_) => unreachable!(),
        });

        self.parent_node = old_parent;

        let tag_id = self.tags_id_counter.inc_one();
        self.tags.insert(tag_id, tags.clone());

        self.nodes
            .insert(def_id, self.mk_node(NodeKind::Item(item), tag_id));

        Some(def_id)
    }

    fn lower_file(&mut self, file: &ir::File) -> File {
        let old_file = self.cur_file.replace(file.id);
        let should_pop = self
            .file_sym_map
            .get(&file.id)
            .map(|block| self.blocks.push(NonNull::from(block)))
            .is_some();

        let f = File {
            items: file
                .items
                .iter()
                .filter_map(|item| self.lower_item(item))
                .collect(),

            file_id: file.id,
            package: ItemPath::from(
                file.package
                    .segments
                    .iter()
                    .map(|i| i.sym.clone())
                    .collect::<Vec<_>>(),
            ),
        };

        if should_pop {
            self.blocks.pop();
        }

        self.cur_file = old_file;
        f
    }
}
