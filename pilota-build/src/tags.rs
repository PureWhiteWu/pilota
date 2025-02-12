use std::{
    any::{Any, TypeId},
    collections::HashMap,
    fmt::Debug,
    hash::Hash,
    ops::{Deref, DerefMut},
};

#[derive(Default, Debug)]
pub struct TypeMap(HashMap<TypeId, Box<dyn Any + Sync + Send>>);

impl TypeMap {
    pub fn insert<T: 'static + Sync + Send>(&mut self, v: T) {
        self.0.insert(TypeId::of::<T>(), Box::new(v));
    }

    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.0
            .get(&TypeId::of::<T>())
            .map(|v| v.downcast_ref().unwrap())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn contains<T: 'static>(&self) -> bool {
        self.0.contains_key(&TypeId::of::<T>())
    }
}

crate::newtype_index!(pub struct TagId { .. });

#[derive(Default, Debug)]
pub struct Tags(TypeMap);

impl Deref for Tags {
    type Target = TypeMap;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Tags {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[macro_export]
macro_rules! tags {
    {
        $($value: expr),*
    } => {
        {
            let mut tags = $crate::tags::Tags::default();
            $(
                tags.insert($value);
            )*
            tags
        }
    };
}

#[macro_export]
macro_rules! new_type {

    {$(#[$attr:meta])* $v:vis struct $name:ident;$($rest:tt)*} => {
        $(#[$attr])*
        $v struct $name;

        $crate::new_type!($($rest)*);
    };

    {$(#[$attr:meta])* $v:vis struct $name:ident(pub $inner:ty);$($rest:tt)*} => {
        $(#[$attr])*
        $v struct $name(pub $inner);

        impl ::std::ops::Deref for $name {
            type Target = $inner;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl ::std::ops::DerefMut for $name {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }

        $crate::new_type!($($rest)*);
    };

    {} => {};
}

pub mod thrift {
    pub struct EntryMessage;
}

pub mod protobuf {

    new_type! {
        #[derive(Debug)]
        pub struct OneOf;

        pub struct Repeated;

        pub struct ClientStreaming;
        pub struct ServerStreaming;

        pub struct SInt32;
        pub struct SInt64;

        pub struct Fixed32;
        pub struct Fixed64;

        pub struct SFixed32;
        pub struct SFixed64;
    }
}
