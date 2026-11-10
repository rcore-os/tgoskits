# ArceOS驱动集成测试

本目录是normal方式的驱动测试程序，实际驱动实现位于仓库根drivers目录。当前仅x86_64 QEMU；Q35、cpu=max、UEFI、512MiB、SMP1、freestanding/无内核TLS。默认一次启动执行两项功能，保留两个单项诊断名称。

```text
drivers/
├── Cargo.toml                  一个测试包：arceos-driver-tests
├── suite.toml                  唯一批次/顺序/成员/入口/所需features
├── build.rs                    读取同一清单，生成所选普通顺序调用
├── src/
│   ├── main.rs                 唯一薄入口，整批结束后退出
│   ├── acpica.rs               原ACPICA测试，run入口
│   └── intel_hda.rs            原HDA测试，run入口
├── build-x86_64-unknown-none.toml
├── qemu-x86_64.toml             共同运行环境及波形捕获
└── README.md

x86-q35                         1次启动，2项
├── acpica-interpreter           AML解释、通知、退出同步
└── intel-hda                    播放、取消、关闭及4096帧输出
```

## 登记与执行

suite.toml顶部run_order决定启动顺序，batches下嵌套tests决定批内顺序。tests登记ID、名称、唯一函数入口及追加features；pcm_waveform声明该成员需要宿主验证输出。Cargo维护真实依赖：目前共同包启用两项所需依赖并集，单项选择只改变生成调用，不取消共同运行能力或设备。

axbuild的test/normal/arceos/drivers.rs负责选择、构建、运行及计划；drivers/catalog.rs由宿主和目标build.rs以普通宿主模块共用，不把测试实现源码包含到宿主。AX_DRIVERS_BATCH/CASES进入构建身份，避免单项/整批混用产物；批次顺序和单项名称只有清单一处登记。

生成的selected_drivers.rs仅位于target构建目录，不提交Git。每项正常返回后才打印DRIVER_TEST_PASS；panic立即中止后续调用。全部结束才输出DRIVERS_BATCH_OK，宿主只接受当前批次/选择对应的最终标记。HDA内部不直接exit，退出由统一main负责。

## 功能与边界

ACPICA运行真实解释器和ArceOS线程/分配器/计时，输入为合成ACPI表/AML，检查初始化唯一性、设备ID/资源解析、Notify回调与退出排空。它不验证真实主板固件或实际ACPI IO/PCI/SCI操作。Engine退出及后台工作排空完成后才开始HDA。

HDA通过公共Playback接口运行QEMU设备，保留配置、提交拷贝、完成顺序、排空、取消和shutdown后的状态检查。原48kHz、双声道、1024帧/周期、4周期保持。内核打印INTEL_HDA_QEMU_OK后，宿主还须检查完整4096帧WAV的格式、声道、内容和顺序，输出PCM_WAVEFORM_OK；只看内核标记不能认定整项成功。

单项ACPICA仍保留共同音频设备和捕获后端，但不调用HDA，不要求不存在的音频输出；选择HDA或整批必须有有效host_pcm_waveform配置并验证。临时WAV在target下独占创建，成功/失败后均清理，不手写第二个-audiodev。波形验证不代表物理声卡可听效果，无需扬声器。

## 命令

在tgoskits根目录：

```bash
# 默认整批，两项共一次启动
cargo xtask arceos test qemu --arch x86_64 --test-group drivers

# 显式批次或单项
cargo xtask arceos test qemu --arch x86_64 --test-group drivers --test-case x86-q35
cargo xtask arceos test qemu --arch x86_64 --test-group drivers --test-case acpica-interpreter
cargo xtask arceos test qemu --arch x86_64 --test-group drivers --test-case intel-hda

# 多选去重，执行顺序仍由清单决定，只启动一次
cargo xtask arceos test qemu --arch x86_64 --test-group drivers --test-case intel-hda,acpica-interpreter,intel-hda

cargo xtask arceos test qemu --arch x86_64 --test-group drivers --list
cargo xtask arceos test qemu --arch x86_64 --test-group drivers --plan
```

plan输出JSON，展示架构、所选成员、features、生成环境/构建身份、配置及是否验证波形。保存文件请写已忽略的target目录。未知、空和不支持架构的显式选择报错，不静默跳过。

CI的drivers组仍使用原ubuntu-base runner和空cache_key，只执行一个共同批次，不重复跑单项。PCI/音频轮询的时间要求不放宽，不改模拟器制造通过；共享宿主长停顿导致的真实失败须记录首错。

## 新增、修改和删除

遵守[启动复用约束](../../../.agents/skills/test-quality/SKILL.md#36-优先复用启动)。新增功能先加入运行条件兼容的批次，所需features取并集；只有明确不兼容或改变被测条件时才新增启动并解释原因。

1. 在src保留唯一实现并提供普通unit返回run入口，正常返回必须意味着检查完成；不得在成员内直接退出整个程序。
2. 在清单登记成员/入口/依赖，更新对应Cargo依赖；不要复制另一份调用名单。涉及新环境时同步batch和run_order及build/qemu配置。
3. 有宿主输出要求的成员必须登记夹具要求，保留单项与整批判定，不能用内核标记替代宿主核验。
4. list/plan核对架构、调用顺序、实际features及启动数，实跑原基线、修改后整批/受影响单项，并注入能被原断言识别的真实错误。
5. 失败应阻止后续成员/整批成功并让外层非零；宿主波形错误即使内核整批已完成也必须失败。恢复后再次通过，保留命令、源码及首错日志。
6. 删除成员时清理独有模块/依赖/夹具；删除批次同步run_order。CI源码路由以声明批次为单位，源文件移动后检查仍被当前组覆盖。
