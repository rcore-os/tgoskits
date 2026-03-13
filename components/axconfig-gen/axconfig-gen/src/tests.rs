use crate::{Config, ConfigErr, ConfigResult, ConfigType, ConfigValue, OutputFormat};

fn check_type_infer(value: &str, expect_ty: &str) -> ConfigResult<()> {
    let value = ConfigValue::new(value)?;
    let expect = ConfigType::new(expect_ty)?;
    let inferred = value.inferred_type()?;
    if inferred != expect {
        println!("inferred: {:?}, expect: {:?}", inferred, expect);
        return Err(super::ConfigErr::ValueTypeMismatch);
    }
    Ok(())
}

macro_rules! assert_err {
    ($res:expr, $err:ident) => {
        match $res {
            Err(ConfigErr::$err) => {}
            _ => panic!("expected `Err({:?})`, got `{:?}`", ConfigErr::$err, $res),
        }
    };
}

#[test]
fn test_type_infer() {
    macro_rules! check_infer {
        ($value:expr, $ty:expr) => {
            check_type_infer($value, $ty).unwrap();
        };
    }

    check_infer!("true", "bool");
    check_infer!("false", "bool");

    check_infer!("0", "uint");
    check_infer!("2333", "uint");
    check_infer!("-2333", "int");
    check_infer!("0b1010", "uint");
    check_infer!("0xdead_beef", "uint");

    check_infer!("\"0xffff_ffff_ffff_ffff\"", "uint");
    check_infer!("\"hello, world!\"", "str");
    check_infer!("\"0o777\"", "uint");
    check_infer!("\"0xx233\"", "str");
    check_infer!("\"\"", "str");

    check_infer!("[1, 2, 3]", "[uint]");
    check_infer!("[\"1\", \"2\", \"3\"]", "[uint]");
    check_infer!("[\"a\", \"b\", \"c\"]", "[str]");
    check_infer!("[true, false, true]", "[bool]");
    check_infer!("[\"0\", \"a\", true, -2]", "(uint, str, bool, int)");
    check_infer!("[]", "?");
    check_infer!("[[]]", "?");
    check_infer!("[[2, 3, 3, 3], [4, 5, 6, 7]]", "[[uint]]");
    check_infer!("[[1], [2, 3], [4, 5, 6]]", "[[uint]]");
    check_infer!(
        "[[2, 3, 3], [4, 5, \"abc\", 7]]",
        "([uint], (uint, uint, str, uint))"
    );
}

#[test]
fn test_type_match() {
    macro_rules! check_match {
        ($value:expr, $ty:expr) => {
            ConfigValue::new_with_type($value, $ty).unwrap();
        };
    }
    macro_rules! check_mismatch {
        ($value:expr, $ty:expr) => {
            assert_err!(ConfigValue::new_with_type($value, $ty), ValueTypeMismatch);
        };
    }

    check_match!("true", "bool");
    check_match!("false", "bool");
    check_mismatch!("true", "int");

    check_match!("0", "uint");
    check_match!("0", "int");
    check_match!("2333", "int");
    check_match!("-2333", "  uint");
    check_match!("0b1010", "int");
    check_match!("0xdead_beef", "int");

    check_mismatch!("\"abc\"", "uint");
    check_match!("\"0xffff_ffff_ffff_ffff\"", "uint");
    check_match!("\"0xffff_ffff_ffff_ffff\"", "str");
    check_match!("\"hello, world!\"", "str");
    check_match!("\"0o777\"", "uint");
    check_match!("\"0xx233\"", "str");
    check_match!("\"\"", "str");

    check_match!("[1, 2, 3]", "[uint]");
    check_match!("[\"1\", \"2\", \"3\"]", "[ uint  ]");
    check_match!("[\"1\", \"2\", \"3\"]", "[str]");
    check_match!("[true, false, true]", "[bool]");
    check_match!("[\"0\", \"a\", true, -2]", "( uint,  str,   bool,  int )");
    check_mismatch!("[\"0\", \"a\", true, -2]", "[uint]  ");
    check_match!("[]", "[int]");
    check_match!("[[]]", "[()]");
    check_match!("[[2, 3, 3, 3], [4, 5, 6, 7]]", "[[uint]]");
    check_match!("[[2, 3, 3, 3], [4, 5, 6, 7]]", "[(int, int, int, int)]");
    check_match!("[[1], [2, 3], [4, 5, 6]]", "[[uint]]");
    check_match!("[[1], [2, 3], [4, 5, 6]]", "([uint],[uint],[uint])");
    check_match!("[[1], [2, 3], [4, 5, 6]]", "((uint),(uint, uint),[uint])");
    check_match!(
        "[[2, 3, 3], [4, 5, \"abc\", 7]]",
        "((int, int, int), (uint, uint, str, uint))"
    );
    check_match!("[[1,2], [3,4], [5,6,7]]", "[[uint]]");
    check_match!("[[1,2], [3,4], [5,6,7]]", "([uint], [uint], [uint])");
    check_match!("[[1,2], [3,4], [5,6,7]]", "((uint,uint), [uint], [uint])");
    check_mismatch!("[[1,2], [3,4], [5,6,7]]", "[(uint, uint)]");
    check_match!("[[[[],[]],[[]]],[]]", "[[[[uint]]]]");
}

#[test]
fn test_err() {
    assert_err!(ConfigType::new("Bool"), InvalidType);
    assert_err!(ConfigType::new("u int"), InvalidType);
    assert_err!(ConfigType::new("usize"), InvalidType);
    assert_err!(ConfigType::new(""), InvalidType);
    assert_err!(ConfigType::new("&str"), InvalidType);
    assert_err!(ConfigType::new("[]"), InvalidType);
    assert_err!(ConfigType::new("(("), InvalidType);
    assert_err!(ConfigType::new("(int,"), InvalidType);
    assert_err!(ConfigType::new("(,)"), InvalidType);
    assert_err!(ConfigType::new("(uint,)"), InvalidType);
    assert_err!(ConfigType::new("[uint, uint]"), InvalidType);
    assert_err!(ConfigType::new("()()"), InvalidType);
    assert_err!(ConfigType::new("(()())"), InvalidType);
    assert!(ConfigType::new("((),())").is_ok());
    assert!(ConfigType::new("(  )").is_ok());
    assert_err!(ConfigValue::new("233.0"), InvalidValue);
}

#[test]
fn test_to_rust() {
    let cfg = r#"[[
        ["0xb000_0000", ["a", "b"], "0x1000_0000"],
        ["0xfe00_0000", ["a", "b"], "0xc0_0000"],
        ["0xfec0_0000", ["a", "b"], "0x1000"],
        ["0xfed0_0000", ["a", "b"], "0x1000"],
        ["0xfee0_0000", ["a", "b"], "0x1000"],
    ]]"#;
    let rust = r#"&[
    &[
        (0xb000_0000, ("a", "b"), 0x1000_0000),
        (0xfe00_0000, ("a", "b"), 0xc0_0000),
        (0xfec0_0000, ("a", "b"), 0x1000),
        (0xfed0_0000, ("a", "b"), 0x1000),
        (0xfee0_0000, ("a", "b"), 0x1000),
    ],
]"#;
    let ty = ConfigType::new("[[(uint, (str, str), uint)]]").unwrap();
    let value = ConfigValue::new(cfg).unwrap();
    assert_eq!(ty.to_rust_type(), "&[&[(usize, (&str, &str), usize)]]");
    assert_eq!(value.to_rust_value(&ty, 0).unwrap(), rust);

    let cfg = r#"[[
        ["0xb000_0000", ["a", "b"], "0x1000_0000"],
        ["0xfe00_0000", ["a", "b"], "0xc0_0000"],
        ["0xfec0_0000", ["a", "b"], "0x1000"],
        ["0xfed0_0000", ["a", "b"], "0x1000"],
        ["0xfee0_0000", ["a", "b", "c"], "0x1000"],
    ]]"#;
    let rust = r#"&[
    &[
        (0xb000_0000, &["a", "b"], 0x1000_0000),
        (0xfe00_0000, &["a", "b"], 0xc0_0000),
        (0xfec0_0000, &["a", "b"], 0x1000),
        (0xfed0_0000, &["a", "b"], 0x1000),
        (0xfee0_0000, &["a", "b", "c"], 0x1000),
    ],
]"#;
    let ty = ConfigType::new("[[(uint, [str], uint)]]").unwrap();
    let value = ConfigValue::new(cfg).unwrap();
    assert_eq!(ty.to_rust_type(), "&[&[(usize, &[&str], usize)]]");
    assert_eq!(value.to_rust_value(&ty, 0).unwrap(), rust);
}

#[test]
fn integration_test() -> std::io::Result<()> {
    let spec = std::fs::read_to_string("../example-configs/defconfig.toml")?;
    let toml = std::fs::read_to_string("../example-configs/output.toml")?;
    let rust = std::fs::read_to_string("../example-configs/output.rs")?;
    let cfg = Config::from_toml(&spec).unwrap();
    assert_eq!(cfg.dump(OutputFormat::Toml).unwrap(), toml);
    assert_eq!(cfg.dump(OutputFormat::Rust).unwrap(), rust);
    Ok(())
}
