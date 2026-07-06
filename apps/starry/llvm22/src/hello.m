// hello.m - minimal Objective-C: a root class with a class method. clang's ObjC front-end
// lowers it to LLVM IR carrying the ObjC metadata (OBJC_CLASS / OBJC_METACLASS symbols) and
// to a native object, exercised without a runtime link.
__attribute__((objc_root_class)) @interface Counter
+ (int)seed;
@end
@implementation Counter
+ (int)seed { return 7; }
@end
int use_counter(void) { return [Counter seed]; }
