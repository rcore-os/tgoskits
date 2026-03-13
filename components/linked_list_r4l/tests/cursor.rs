use linked_list_r4l::*;
use std::{fmt, sync::Arc};

def_node! {
    struct Node(String);
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.inner.eq(other.inner())
    }
}
impl PartialEq<str> for Node {
    fn eq(&self, other: &str) -> bool {
        self.inner.eq(other)
    }
}
impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node").field("inner", &self.inner).finish()
    }
}

#[test]
fn cursor_front_mut() {
    let mut list = List::<Box<Node>>::new();
    list.push_back(Box::new(Node::new("hello".to_owned())));
    list.push_back(Box::new(Node::new("world".to_owned())));

    let mut cursor = list.cursor_front_mut();
    assert_eq!(cursor.current().unwrap(), "hello");

    // SAFETY: we have unique access through Box
    unsafe {
        cursor.peek_next().unwrap().inner.push('!');
    }

    cursor.move_next();

    // SAFETY: we have unique access through Box
    unsafe {
        assert_eq!(cursor.current_mut().unwrap(), "world!");
        cursor.peek_prev().unwrap().inner = "Hello".to_owned();
    }

    // `CommonCursor::move_next` stops at None when it reaches the end of list,
    // because `raw_list::Iterator` stops at None.
    cursor.move_next();
    assert_eq!(cursor.current(), None);

    // Then restart from the head.
    cursor.move_next();
    assert_eq!(cursor.current().unwrap(), "Hello");
}

#[test]
fn cursor_front() {
    let mut list = List::<Arc<Node>>::new();
    list.push_back(Arc::new(Node::new("hello".to_owned())));
    list.push_back(Arc::new(Node::new("world".to_owned())));

    let mut cursor = list.cursor_front();
    assert_eq!(cursor.peek_next().unwrap(), "world");
    assert_eq!(cursor.current().unwrap(), "hello");
    cursor.move_next();
    assert_eq!(cursor.current().unwrap(), "world");
    assert_eq!(cursor.peek_prev().unwrap(), "hello");

    cursor.move_next();
    assert_eq!(cursor.current(), None);

    cursor.move_next();
    assert_eq!(cursor.current().unwrap(), "hello");
}

#[test]
fn insert_after() {
    let mut list = List::<Box<Node>>::new();
    list.push_back(Box::new(Node::new("Hello".to_owned())));

    let existing = list.cursor_front().current_ptr().unwrap();
    let data = Box::new(Node::new("world".to_owned()));
    unsafe {
        assert!(list.insert_after(existing, data));
    }

    let mut cursor = list.cursor_front_mut();
    let data = Box::new(Node::new(", ".to_owned()));
    assert!(cursor.insert_after(data));

    cursor.move_next(); // ", "
    cursor.move_next(); // "world"
    let data = Box::new(Node::new("!".to_owned()));
    assert!(cursor.insert_after(data));

    cursor.move_next(); // "!"
    cursor.move_next(); // end
    assert_eq!(cursor.current(), None);

    assert_eq!(&*v_list(list.iter()), ["Hello", ", ", "world", "!"]);
}

fn v_list<'a>(iter: impl IntoIterator<Item = &'a Node>) -> Box<[&'a str]> {
    iter.into_iter().map(|node| node.inner.as_str()).collect()
}

#[test]
fn remove() {
    let mut list = List::<Arc<Node>>::new();
    let hello = Arc::new(Node::new("hello".to_owned()));
    list.push_back(hello.clone());
    let world = Arc::new(Node::new("world".to_owned()));
    list.push_back(world);

    unsafe {
        assert_eq!(&*list.remove(&hello).unwrap(), "hello");
    }

    let mut cursor = list.cursor_front_mut();
    let world = cursor.remove_current().unwrap();

    assert_eq!(cursor.current(), None);

    list.push_back(hello);
    list.push_back(world);
    assert_eq!(&*v_list(list.iter()), ["hello", "world"]);
}
