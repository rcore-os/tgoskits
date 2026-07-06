// hello.cpp - clang++ C++ front-end: templates, a class with methods, range-based
// for, and the STL (std::vector / std::string). Deterministic stdout for exact
// assertion: "CPP22 SUM=15 CNT=5".
#include <cstdio>
#include <vector>
#include <string>

template <typename T>
static T add(T a, T b) { return a + b; }

struct Counter {
    int n;
    Counter() : n(0) {}
    void bump() { ++n; }
    int get() const { return n; }
};

int main() {
    std::vector<int> v = {1, 2, 3, 4, 5};
    int s = 0;
    for (int x : v) s = add(s, x);
    Counter c;
    for (int i = 0; i < 5; ++i) c.bump();
    std::string tag = "CPP22";
    printf("%s SUM=%d CNT=%d\n", tag.c_str(), s, c.get());
    return 0;
}
