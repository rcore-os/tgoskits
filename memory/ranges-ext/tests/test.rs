use ranges_ext::{
    VecOp,
    test_helper::{RangeKind, TestRange},
};

#[test]
fn test_merge_same_kind() {
    // 定义测试用例：(描述, 输入ranges, 期望输出ranges)
    let test_cases: &[(&str, Vec<TestRange>, Vec<TestRange>)] = &[
        // 1. 两个相邻的相同kind的range应该合并
        (
            "adjacent same kind",
            vec![
                TestRange::new(0, 10, RangeKind::TypeA),
                TestRange::new(10, 20, RangeKind::TypeA),
            ],
            vec![TestRange::new(0, 20, RangeKind::TypeA)],
        ),
        // 2. 两个重叠的相同kind的range应该合并
        (
            "overlapping same kind",
            vec![
                TestRange::new(0, 15, RangeKind::TypeA),
                TestRange::new(10, 20, RangeKind::TypeA),
            ],
            vec![TestRange::new(0, 20, RangeKind::TypeA)],
        ),
        // 3. 两个分离的相同kind的range不应该合并
        (
            "separated same kind",
            vec![
                TestRange::new(0, 10, RangeKind::TypeA),
                TestRange::new(20, 30, RangeKind::TypeA),
            ],
            vec![
                TestRange::new(0, 10, RangeKind::TypeA),
                TestRange::new(20, 30, RangeKind::TypeA),
            ],
        ),
        // 4. 两个不同kind的range不应该合并（即使重叠）
        (
            "different kinds overlapping",
            vec![
                TestRange::new(0, 15, RangeKind::TypeA),
                TestRange::new(10, 20, RangeKind::TypeB),
            ],
            vec![
                TestRange::new(0, 15, RangeKind::TypeA),
                TestRange::new(10, 20, RangeKind::TypeB),
            ],
        ),
        // 5. 多个可以合并的相同kind的range
        (
            "multiple mergeable same kind",
            vec![
                TestRange::new(0, 10, RangeKind::TypeA),
                TestRange::new(10, 20, RangeKind::TypeA),
                TestRange::new(20, 30, RangeKind::TypeA),
            ],
            vec![TestRange::new(0, 30, RangeKind::TypeA)],
        ),
        // 6. 空集合
        ("empty", vec![], vec![]),
        // 7. 单个range
        (
            "single range",
            vec![TestRange::new(0, 10, RangeKind::TypeA)],
            vec![TestRange::new(0, 10, RangeKind::TypeA)],
        ),
        // 8. 复杂混合：多种类型，部分可合并
        (
            "complex mixed",
            vec![
                TestRange::new(0, 10, RangeKind::TypeA),
                TestRange::new(10, 20, RangeKind::TypeA),
                TestRange::new(5, 15, RangeKind::TypeB),
                TestRange::new(30, 40, RangeKind::TypeA),
                TestRange::new(15, 25, RangeKind::TypeB),
            ],
            vec![
                TestRange::new(0, 20, RangeKind::TypeA),
                TestRange::new(5, 25, RangeKind::TypeB),
                TestRange::new(30, 40, RangeKind::TypeA),
            ],
        ),
        // 9. 完全重叠的range
        (
            "fully overlapping same kind",
            vec![
                TestRange::new(0, 20, RangeKind::TypeA),
                TestRange::new(5, 15, RangeKind::TypeA),
            ],
            vec![TestRange::new(0, 20, RangeKind::TypeA)],
        ),
        // 10. 反向顺序的相邻range
        (
            "reverse order adjacent",
            vec![
                TestRange::new(10, 20, RangeKind::TypeA),
                TestRange::new(0, 10, RangeKind::TypeA),
            ],
            vec![TestRange::new(0, 20, RangeKind::TypeA)],
        ),
        // 11. 多个不同类型，每种类型内部可合并
        (
            "multiple types each mergeable",
            vec![
                TestRange::new(0, 10, RangeKind::TypeA),
                TestRange::new(10, 20, RangeKind::TypeA),
                TestRange::new(0, 10, RangeKind::TypeB),
                TestRange::new(10, 20, RangeKind::TypeB),
                TestRange::new(0, 10, RangeKind::TypeC),
                TestRange::new(10, 20, RangeKind::TypeC),
            ],
            vec![
                TestRange::new(0, 20, RangeKind::TypeA),
                TestRange::new(0, 20, RangeKind::TypeB),
                TestRange::new(0, 20, RangeKind::TypeC),
            ],
        ),
        // 12. 刚好接触的边界（end == start）
        (
            "touching boundaries",
            vec![
                TestRange::new(0, 5, RangeKind::TypeA),
                TestRange::new(5, 10, RangeKind::TypeA),
                TestRange::new(10, 15, RangeKind::TypeA),
            ],
            vec![TestRange::new(0, 15, RangeKind::TypeA)],
        ),
    ];

    for (description, input, expected) in test_cases {
        let mut vec: Vec<TestRange> = input.clone();
        vec.merge_same_kind();
        let result: Vec<TestRange> = vec;

        // 比较结果，不考虑顺序
        assert_eq!(
            result.len(),
            expected.len(),
            "Test case '{}' failed: length mismatch. Got {} ranges, expected {}",
            description,
            result.len(),
            expected.len()
        );

        for exp in expected {
            assert!(
                result.contains(exp),
                "Test case '{}' failed: expected range {:?} not found in result {:?}",
                description,
                exp,
                result
            );
        }

        for res in &result {
            assert!(
                expected.contains(res),
                "Test case '{}' failed: unexpected range {:?} in result. Expected: {:?}",
                description,
                res,
                expected
            );
        }

        println!("✓ Test case '{}' passed", description);
    }
}

#[test]
fn test_merge_same_kind_idempotent() {
    // 测试幂等性：多次调用merge_same_kind应该得到相同结果
    let mut vec1 = vec![
        TestRange::new(0, 10, RangeKind::TypeA),
        TestRange::new(10, 20, RangeKind::TypeA),
        TestRange::new(5, 15, RangeKind::TypeB),
    ];

    vec1.merge_same_kind();
    let result1 = vec1.clone();

    let mut vec2 = result1.clone();
    vec2.merge_same_kind();
    let result2 = vec2;

    assert_eq!(result1, result2, "merge_same_kind should be idempotent");
}

#[cfg(feature = "alloc")]
#[test]
fn test_alloc_vec_impl() {
    use ranges_ext::VecOp;

    // 测试 alloc::vec::Vec 的实现
    let mut vec: Vec<TestRange> = vec![
        TestRange::new(0, 10, RangeKind::TypeA),
        TestRange::new(10, 20, RangeKind::TypeA),
        TestRange::new(5, 15, RangeKind::TypeB),
    ];

    vec.merge_same_kind();

    // 应该合并成两个range
    assert_eq!(vec.len(), 2);

    // 验证合并结果
    assert!(
        vec.contains(&TestRange::new(0, 20, RangeKind::TypeA))
            || vec.contains(&TestRange::new(5, 15, RangeKind::TypeB))
    );

    println!("✓ alloc::vec::Vec implementation test passed");
}

#[test]
fn test_heapless_vec_impl() {
    use heapless::Vec as HeaplessVec;
    use ranges_ext::VecOp;

    // 测试 heapless::Vec 的实现
    let mut vec: HeaplessVec<TestRange, 10> = HeaplessVec::new();
    vec.push(TestRange::new(0, 10, RangeKind::TypeA)).unwrap();
    vec.push(TestRange::new(10, 20, RangeKind::TypeA)).unwrap();
    vec.push(TestRange::new(5, 15, RangeKind::TypeB)).unwrap();

    vec.merge_same_kind();

    // 应该合并成两个range
    assert_eq!(vec.len(), 2);

    // 验证合并结果
    assert!(
        vec.as_slice()
            .contains(&TestRange::new(0, 20, RangeKind::TypeA))
            || vec
                .as_slice()
                .contains(&TestRange::new(5, 15, RangeKind::TypeB))
    );

    println!("✓ heapless::Vec implementation test passed");
}

#[test]
fn test_heapless_vec_capacity_error() {
    use heapless::Vec as HeaplessVec;
    use ranges_ext::{RangeError, VecOp};

    // 测试容量限制错误
    let mut vec: HeaplessVec<TestRange, 2> = HeaplessVec::new();
    vec.push(TestRange::new(0, 10, RangeKind::TypeA)).unwrap();
    vec.push(TestRange::new(10, 20, RangeKind::TypeA)).unwrap();

    // 尝试插入第三个元素应该返回容量错误
    let result = VecOp::push(&mut vec, TestRange::new(20, 30, RangeKind::TypeA));
    assert!(matches!(result, Err(RangeError::Capacity)));

    println!("✓ heapless::Vec capacity error test passed");
}

#[test]
fn test_merge_add() {
    use ranges_ext::{VecOp, test_helper::RangeKind};

    // 测试用例集合
    let test_cases: &[(&str, Vec<TestRange>, TestRange, Result<Vec<TestRange>, ()>)] = &[
        // 1. 完全覆盖：新range完全覆盖旧range（可覆盖）
        (
            "complete overlap - overwritable",
            vec![TestRange::new(10, 20, RangeKind::TypeA)],
            TestRange::new(5, 25, RangeKind::TypeB),
            Ok(vec![TestRange::new(5, 25, RangeKind::TypeB)]),
        ),
        // 2. 中间分割：新range在旧range中间（可覆盖）
        (
            "middle split - overwritable",
            vec![TestRange::new(0, 30, RangeKind::TypeA)],
            TestRange::new(10, 20, RangeKind::TypeB),
            Ok(vec![
                TestRange::new(0, 10, RangeKind::TypeA),
                TestRange::new(20, 30, RangeKind::TypeA),
                TestRange::new(10, 20, RangeKind::TypeB),
            ]),
        ),
        // 3. 左侧覆盖：新range覆盖旧range左侧（可覆盖）
        (
            "left overlap - overwritable",
            vec![TestRange::new(10, 30, RangeKind::TypeA)],
            TestRange::new(5, 20, RangeKind::TypeB),
            Ok(vec![
                TestRange::new(20, 30, RangeKind::TypeA),
                TestRange::new(5, 20, RangeKind::TypeB),
            ]),
        ),
        // 4. 右侧覆盖：新range覆盖旧range右侧（可覆盖）
        (
            "right overlap - overwritable",
            vec![TestRange::new(10, 30, RangeKind::TypeA)],
            TestRange::new(20, 40, RangeKind::TypeB),
            Ok(vec![
                TestRange::new(10, 20, RangeKind::TypeA),
                TestRange::new(20, 40, RangeKind::TypeB),
            ]),
        ),
        // 5. 无重叠：直接添加并可能合并
        (
            "no overlap",
            vec![TestRange::new(0, 10, RangeKind::TypeA)],
            TestRange::new(10, 20, RangeKind::TypeA),
            Ok(vec![TestRange::new(0, 20, RangeKind::TypeA)]),
        ),
        // 6. 多个重叠：处理多个可覆盖的range
        (
            "multiple overlaps - overwritable",
            vec![
                TestRange::new(0, 15, RangeKind::TypeA),
                TestRange::new(20, 35, RangeKind::TypeA),
            ],
            TestRange::new(10, 30, RangeKind::TypeB),
            Ok(vec![
                TestRange::new(0, 10, RangeKind::TypeA),
                TestRange::new(30, 35, RangeKind::TypeA),
                TestRange::new(10, 30, RangeKind::TypeB),
            ]),
        ),
    ];

    for (description, initial, new_item, expected) in test_cases {
        let mut vec: Vec<TestRange> = initial.clone();
        let result = vec.merge_add(new_item.clone());

        match expected {
            Ok(expected_vec) => {
                assert!(
                    result.is_ok(),
                    "Test case '{}' failed: expected Ok, got {:?}",
                    description,
                    result
                );

                assert_eq!(
                    vec.len(),
                    expected_vec.len(),
                    "Test case '{}' failed: length mismatch. Got {} ranges, expected {}",
                    description,
                    vec.len(),
                    expected_vec.len()
                );

                for exp in expected_vec {
                    assert!(
                        vec.contains(exp),
                        "Test case '{}' failed: expected range {:?} not found in result {:?}",
                        description,
                        exp,
                        vec
                    );
                }

                println!("✓ Test case '{}' passed", description);
            }
            Err(_) => {
                assert!(
                    result.is_err(),
                    "Test case '{}' failed: expected Err, got Ok",
                    description
                );
                println!("✓ Test case '{}' passed (error expected)", description);
            }
        }
    }
}

#[test]
fn test_merge_add_conflict() {
    use ranges_ext::{
        RangeError, VecOp,
        test_helper::{RangeKind, TestRange},
    };

    // 测试不可覆盖的冲突
    let mut vec: Vec<TestRange> = vec![TestRange::new_with_overwritable(
        10,
        20,
        RangeKind::TypeA,
        false, // 不可覆盖
    )];

    let new_item = TestRange::new(15, 25, RangeKind::TypeB);
    let result = vec.merge_add(new_item.clone());

    assert!(
        matches!(result, Err(RangeError::Conflict { .. })),
        "Expected Conflict error, got {:?}",
        result
    );

    println!("✓ merge_add conflict test passed");
}
