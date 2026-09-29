# The default font

`SourceHanSansSC-Regular-Subset.otf` — 思源黑体（Source Han Sans SC）Regular，裁到本系统实际会
画的 **4008 个字符**：

```
U+0020–U+007E     ASCII 可打印字符
GB2312 一级汉字    区 16–55，共 3755 字
中文标点与符号      、。〈〉《》「」【】……—～·×÷±° 等
U+FF01–U+FF5E     全角形式
```

**1.8 MB**，进 `.rodata`（flash，不占 RAM）。对比：这个 OS 之前用的是 16 KiB 的 Roboto 拉丁子
集，而 iced 自带的后备字体是 **441 KiB** 的 Fira Sans（正因为没人想要它进固件镜像，才挂在特
征后面）。

它由 [`fonts::install_default`](../src/fonts.rs) 安装 —— 当 app 或固件自己没有装字体时。这样
一个为 iced 写的、对本板字体一无所知的程序，仍然能画出文字，而且是中文。

**为什么换掉了 Roboto**：整个系统只有一种默认字体（`Font::default()` 只能指向一个字族），而
中英混排时把拉丁与中文分成两个字体，意味着每个标签都要决定两次由谁来画。一份同时覆盖中英的
字体更简单，也让字重与行高在两种文字之间保持一致。

## 设备上的字形从哪来

排版（advance、kerning、断行）始终由 cosmic-text 用这份字体在设备上完成，**字体文件必须留在
设备上**。被烘掉的只有「像素覆盖」：构建机上预先把字形光栅化成位图，设备上查表即可 —— 因为
ESP32-S3 上现场光栅化一个字形约 **1.6 ms**（打开设置页 147 个字形 = 238 ms，占那一帧 400 ms
的六成）。

| 字号 | 来源 | 说明 |
| ---: | --- | --- |
| 14、15、18 | **烘焙表**（同目录的 `*-common@{14,15,18}px.bin`） | app 的**正文**就这三档：设置/计算器/播放器的次要文本与数值、启动器标签与状态栏、终端正文、计数器的标签与页脚 |
| 32、34、38、96 | 设备上按需光栅化 | 只剩**展示**字号（标题带、计算器主显示、磁贴符号、计数器数字）。它们只画几个字，且卡片尺寸是围绕它们排的；满字符集烘一档要 3–34 MB，不划算 |
| 其他任意字号 | 设备上按需光栅化 | 新加的 app 可以用任何字号 —— 只是那一屏每个新字形慢一次（约 1.6 ms），之后进 RAM 缓存 |

**没有烘焙的字号不会出错**：只是那一屏（每个新字形、每档字号一次）慢一点。所以 app 里加一个
字号是允许的；当它承载大量文字时，才值得烘。烘好的清单与体积见
[`assets/fonts/baked/MANIFEST.md`](../../../assets/fonts/baked/MANIFEST.md)。

## How it was made

源字体是思源黑体 SC Regular，取自 Adobe 的发布页：

```bash
curl -sSL -o assets/fonts/source/source-han-sans-sc-regular.otf \
  https://github.com/adobe-fonts/source-han-sans/raw/release/OTF/SimplifiedChinese/SourceHanSansSC-Regular.otf
# 16 529 832 B
# sha256 f1d8611151880c6c336aabeac4640ef434fa13cbfbf1ffe82d0a71b2a5637256
```

（源文件本身不进仓库：16.5 MB，一条命令就能重现，见 `.gitignore`。）

字符集由 `pomelo-os` 的 `assets/fonts/source/charset-common.txt` 给出，裁成运行时字体：

```bash
pyftsubset assets/fonts/source/source-han-sans-sc-regular.otf \
    --text-file=assets/fonts/source/charset-common.txt \
    --drop-tables+=DSIG \
    --output-file=vendor/iced-pomelo-winit/fonts/SourceHanSansSC-Regular-Subset.otf
```

`--name-IDs` 没有收窄（保留全部 name 记录）：`install` 要从 face 里读出家族名并把它设为
sans-serif 族，名字被裁掉就会静默落到 iced 的图标字体上 —— 这正是 [`fonts`](../src/fonts.rs)
模块存在的理由。

烘焙表由 `pomelo-os` 的 `tools/bake_glyphs.py` 生成（格式、字号选择理由都在那个文件里）。

## Licence

思源黑体以 **SIL Open Font License 1.1** 发布 —— 全文见同目录的 `OFL.txt`，字体内部也带
name 记录 13/14 的声明。Copyright 2014–2025 Adobe (<http://www.adobe.com/>), with Reserved
Font Name 'Source'. OFL 允许子集、内嵌与再分发，因此这份子集可以随系统一起发布。
