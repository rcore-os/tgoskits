#!/bin/sh
# 自动生成: 运行 usertestsuit 所有测试

export HOME=/root
echo "=== usertestsuit on StarryOS ==="

echo "--- Running: open_test ---"
/bin/usertestsuit/open_test
echo "--- End: open_test ---"

echo "--- Running: read_test ---"
/bin/usertestsuit/read_test
echo "--- End: read_test ---"

echo "--- Running: write_test ---"
/bin/usertestsuit/write_test
echo "--- End: write_test ---"

echo ""
echo "=== All usertestsuit tests completed ==="
