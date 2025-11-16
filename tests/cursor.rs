use linked_list_r4l::*;
use std::sync::Arc;

def_node! {
    struct Node(String);
}

#[test]
fn cursor_front_mut() {
    let mut list = List::<Box<Node>>::new();
    list.push_back(Box::new(Node::new("hello".to_owned())));
    list.push_back(Box::new(Node::new("world".to_owned())));

    let mut cursor = list.cursor_front_mut();
    unsafe {
        assert_eq!(cursor.current().unwrap().inner(), "hello");

        cursor.peek_next().unwrap().inner.push('!');
        cursor.move_next();
        assert_eq!(cursor.current().unwrap().inner(), "world!");

        cursor.peek_prev().unwrap().inner = "Hello".to_owned();

        // `CommonCursor::move_next` stops at None when it reaches the end of list,
        // because `raw_list::Iterator` stops at None.
        cursor.move_next();
        assert_eq!(cursor.current().map(|_| ()), None);

        // Then restart from the head.
        cursor.move_next();
        assert_eq!(cursor.current().unwrap().inner(), "Hello");
    }
}

#[test]
fn cursor_front() {
    let mut list = List::<Arc<Node>>::new();
    list.push_back(Arc::new(Node::new("hello".to_owned())));
    list.push_back(Arc::new(Node::new("world".to_owned())));

    let mut cursor = list.cursor_front();
    assert_eq!(cursor.peek_next().unwrap().inner(), "world");
    assert_eq!(cursor.current().unwrap().inner(), "hello");
    cursor.move_next();
    assert_eq!(cursor.current().unwrap().inner(), "world");
    assert_eq!(cursor.peek_prev().unwrap().inner(), "hello");

    cursor.move_next();
    assert_eq!(cursor.current().map(|_| ()), None);

    cursor.move_next();
    assert_eq!(cursor.current().unwrap().inner(), "hello");
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
    assert_eq!(cursor.current_ptr(), None);

    let val: Box<[_]> = list.iter().map(|node| node.inner.as_str()).collect();
    assert_eq!(&*val, ["Hello", ", ", "world", "!"]);
}
