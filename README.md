# LinkedList

[![Crates.io](https://img.shields.io/crates/v/linked_list_r4l)](https://crates.io/crates/linked_list_r4l)
[![Doc.rs](https://docs.rs/linked_list_r4l/badge.svg)](https://docs.rs/linked_list_r4l)
[![CI](https://github.com/arceos-org/linked_list_r4l/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/linked_list_r4l/actions/workflows/ci.yml)

 Linked lists that supports arbitrary removal in constant time.

 It is based on the linked list implementation in [Rust-for-Linux][1].

 [1]: https://github.com/Rust-for-Linux/linux/blob/rust/rust/kernel/linked_list.rs

## Examples 

 ```rust
 use linked_list_r4l::{GetLinks, Links, List};

 type InnerType = usize;

 pub struct ExampleNode {
     pub inner: InnerType,
     links: Links<Self>,
 }

 impl GetLinks for ExampleNode {
     type EntryType = Self;

     fn get_links(t: &Self) -> &Links<Self> {
         &t.links
     }
 }

 impl ExampleNode {
     fn new(inner: InnerType) -> Self {
         Self {
             inner,
             links: Links::new()
         }
     }

     fn inner(&self) -> &InnerType {
         &self.inner
     }
 }

 let node1 = Box::new(ExampleNode::new(0));
 let node2 = Box::new(ExampleNode::new(1));
 let mut list =  List::<Box<ExampleNode>>::new();

 list.push_back(node1);
 list.push_back(node2);

 // Support Iter
 for (i,e) in list.iter().enumerate() {
     assert!(*e.inner() == i);
 }

 // Pop drop
 assert!(*list.pop_front().unwrap().inner() == 0);
 assert!(*list.pop_front().unwrap().inner() == 1);

 ```


