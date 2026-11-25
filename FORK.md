# نسخة Xbox من VI Translator

هذه نسخة مشتقة (fork) من [VI Translator](https://github.com/vi-translator/vi-translator-desktop)، هدفها:
- دعم Xbox.
- قياس الأداء وتحسينه.

وتحافظ على:
- كل مزايا التطبيق الأصلي.
- ذاكرة الترجمة المحلية والمشتركة واستمرار تحديثاتها.
- إمكانية دمج تحديثات الأصل باستمرار.

الترخيص هو ترخيص الأصل نفسه (AGPL-3.0-only)، والإسناد في [`NOTICE`](NOTICE).

- **الأساس الحالي:** الأصل عند الإيداع المسجّل في [`.fork/base-commit`](.fork/base-commit)، وهو حاليًا v1.4.4 (`b46e280`).
- **المراحل المنفذة:** المرحلة 0 (القياس) والمرحلة 1 (حماية النسخة).

## المبادئ الملزمة

1. **ملفات لا تُعدَّل إطلاقًا:**
   - `core/src/memory.rs` و`core/src/sync.rs`: الذاكرة وتنزيلاتها، والخادم، ومفاتيح الحزم، ورتب المصادر.
   - `core/src/sign.rs`: مفتاح التحقق من التحديثات والسكربتات.
   - ويبقى كذلك:
     - استعمال المفتاح في `pagescript.rs` و`updater.rs`.
     - معرّف التطبيق `IDENTIFIER`، أي مجلد بياناته.

   يتحقق من ذلك [`.fork/guard.sh`](.fork/guard.sh) مقارنةً بالأساس، في:
   - كل دفع (سير عمل Fork guard).
   - كل بناء.
   - كل مزامنة.
2. **مكان كود النسخة:** في ملفات جديدة. أما ملفات الأصل فلا يدخلها إلا خطافات قصيرة تبدأ بتعليق `// fork:`، وجردها أدناه.
3. **المفاتيح:** كل إضافة تعمل خلف مفتاح في `fork-settings.json`، وكل المفاتيح مطفأة افتراضيًا. الملف منفصل عن إعدادات الأصل `config.json`.
4. **رقم الإصدار:** إصدار التطبيق يبقى إصدار الأصل الذي بُنيت عليه النسخة، لأن سكربتات الصفحات المنزّلة تتحقق منه.
5. **تحديثات الأصل:** يُكتفى بالإعلان عنها داخل التطبيق ولا تُثبَّت، لأن تثبيتها يستبدل هذه النسخة ودعم Xbox معها. تصل مدموجة عبر سير عمل المزامنة.

## الملفات الجديدة

| الملف | الغرض |
|---|---|
| `core/src/fork/mod.rs` | وحدة النسخة في النواة |
| `core/src/fork/settings.rs` | مفاتيح النسخة (`fork-settings.json`) |
| `core/src/fork/linelog.rs` | سجل القياس السطري (`lines.log`) وأسباب الفقد |
| `core/src/fork/tv_timing.rs` | زمن التلفاز: `tv_sent` و`tv_drawn` |
| `core/tests/fork_measurement.rs` | اختبار أن السجل لا يغيّر شيئًا مما يُعرض |
| `core/examples/fork_memory_equivalence.rs` | أداة تكافؤ الذاكرة |
| `src-tauri/src/fork_cmds.rs` | أوامر النافذة: المفاتيح، ومجلد السجل، وصفحة تحديث الأصل |
| `src-tauri/src/replay.rs` | أدوات القياس: `--replay-video` و`--evaluate` و`--gold-draft` |
| `ui/fork.js` | إضافات النافذة: كلماتها العربية، ومفتاح السجل، وشريط التحديث |
| `.fork/base-commit` | الإيداع الأصلي الذي بُنيت عليه النسخة |
| `.fork/guard.sh` | حارس الملفات المحمية |
| `.fork/memory-check.cmd` | فحص تكافؤ ذاكرتك على Windows |
| `.github/workflows/fork-guard.yml` | الحارس + اختبارات النواة + تكافؤ الذاكرة، عند كل دفع |
| `.github/workflows/fork-build.yml` | بناء Windows x64 يدويًا |
| `.github/workflows/upstream-sync.yml` | مزامنة تحديثات الأصل أسبوعيًا |
| `FORK.md`، `NOTICE` | هذا الدليل والإسناد |

## جرد الخطافات في ملفات الأصل

| الملف | الخطافات |
|---|---|
| `core/src/lib.rs` | `pub mod fork;` |
| `core/src/engine.rs` | أحداث السجل في مسار السطر، وكلها لا تعمل إلا والسجل مفعّل:<br>• الظهور الأول `seen`، والإرسال `sent`، والترجمة `translated`، والتخطي `skipped`.<br>• الاستبدال قبل العرض `replaced`، والإسقاط `dropped`، والعرض `shown`، والإخفاء `hidden`.<br>• المسح `cleared`، والصفحات `page`، وأسباب الترشيح `filtered`، وأزمنة القراءة `reads`. |
| `core/src/tv_server.rs` | المسار `/tv/drawn` (للتلفاز المرتبط فقط)، وترويسة `X-Ack`، وتسجيل `tv_sent` |
| `src-tauri/src/capture.rs` | زمن التقاط كل إطار (`note_frame`) |
| `src-tauri/src/main.rs` | `mod fork_cmds;`، و`mod replay;`، وأوامر القياس في سطر الأوامر، وتطبيق المفاتيح عند البدء، والإعلان فقط عن تحديثات الأصل في `install_update` |
| `ui/index.html` | تحميل `fork.js` |
| `ui/tv.html` | إبلاغ الكمبيوتر بعد رسم كل حالة، ما دام السجل مفعّلًا |

لعرض الجرد الكامل بأرقام الأسطر:

```
git grep -n "fork:" -- core/src/engine.rs core/src/lib.rs core/src/tv_server.rs src-tauri/src/capture.rs src-tauri/src/main.rs ui/index.html ui/tv.html
```

## المفاتيح: `fork-settings.json`

يوجد في مجلد البيانات `%APPDATA%\com.vitranslator.app`. إن غاب الملف أو تلف تُطبَّق القيم الافتراضية، ولا يُكتب شيء.

| المفتاح | الافتراضي | المعنى |
|---|---|---|
| `line_log` | `false` | سجل القياس `lines.log` |
| `line_log_mb` | `1` | حجم السجل بالميغابايت (1–64)؛ يُحتفظ بالملف ونسخة أقدم واحدة (`lines.log.1`) |
| `install_upstream_updates` | `false` | `true` يعيد لزر «التحديث الآن» تثبيت إصدار الأصل، فيستبدل هذه النسخة ودعم Xbox معها |

## سجل القياس `lines.log`

- **التفعيل:** الإعدادات ← Debugging ← «سجل القياس (lines.log)». ومن هناك يُضبط حجمه ويُفتح مجلده.
- **المكان:** `%APPDATA%\com.vitranslator.app\lines.log`، بصيغة JSON: حدث واحد في كل سطر، وفيه دائمًا `t` (زمن يونكس بالمللي ثانية) و`ev` (نوع الحدث).
- **الأثر على التطبيق:** للقياس فقط، فلا يغيّر شيئًا مما يُقرأ أو يُعرض. يثبت ذلك اختبار `fork_the_measurement_log_changes_nothing_shown_and_tells_each_lines_way`، الذي يقارن المعروض مع السجل مفعّلًا ومطفأً.
- **الخصوصية:**
  - مفتاح التلفاز لا يُسجَّل، بل بصمته المختصرة فقط.
  - النصوص مقتطعة إلى 300 حرف.

| الحدث | متى | أهم الحقول |
|---|---|---|
| `seen` | قراءة جديدة لمنطقة (الظهور الأول أو تغيّر) | `area`، `text`، `frame`، `captured` (زمن التقاط الإطار)، `ocr_ms`، `sure` |
| `sent` | السطر ذهب إلى المترجم | `area`، `seq`، `en`، `parts`، `at` (بداية ظهور الإنجليزية) |
| `translated` | وصلت الترجمة | `ok`، `hit` (من الذاكرة)، `ms`، `sources` (مصدر كل جزء)، `error_kind` |
| `skipped` | لم يُترجم لأن سطرًا أحدث في المنطقة سبقه | `reason: superseded` |
| `replaced` | ترجمة كانت تنتظر العرض استبدلها أحدث منها | `seq`، `by` |
| `dropped` | رُفضت لأن كل المعروض مُسح بعد قراءتها | `reason: cleared` |
| `shown` | ظهرت الترجمة | `seq`، `at`، `late` (الإنجليزية اختفت قبلها) |
| `hidden` | أُخفيت | `shown_ms` (مدة البقاء) |
| `cleared` | مُسح كل المعروض | `reason`: `language` أو `page` |
| `page` | ظهرت صفحة لعبة أو اختفت | `id`، `on` |
| `filtered` | نص قُرئ وتُرك، مع سببه | `reason`: `clean` (أحرف أقل من سطر)، `not_centered`، `no_bottom_line`، `help_anchor`، `help_not_sentence`، `not_words`، `reviewed_only` |
| `reads` | ملخص كل ثانية لأزمنة القراءة | `n`، `ocr_avg`، `ocr_max`، `pass_avg`، `pass_max`، `areas`، `suppressed` |
| `tv_sent` | كُتب رد `tv.json` لتلفاز | `tv`، `h` (الإصدار)، `shown` |
| `tv_drawn` | التلفاز رسم الحالة | `sent`، `recv`، `draw_ms`، `est` (تقدير لحظة الرسم) |

الحدثان `filtered` و`reads` مُقلَّلان حتى لا يكبر السجل:
- النص نفسه لا يُكرَّر خلال 5 ثوانٍ.
- الحد الأقصى 20 حدثًا في الثانية، والفائض يُعَدّ في `suppressed`.

## أدوات القياس (سطر الأوامر)

- **المتطلبات:** FFmpeg، أي `ffmpeg` و`ffprobe` في PATH (مثلًا: `winget install Gyan.FFmpeg`).
- **التشغيل:** التطبيق برنامج نوافذ، لذلك يُشغَّل من PowerShell مع تمرير المخرجات حتى ينتظره PowerShell ويعرض نتيجته:

```powershell
& .\vi-translator-xbox-windows-x64.exe --replay-video clip.mp4 --gold gold.json --report report.json 2>&1 | Out-Host
```

- **`--replay-video <video> [--gold gold.json] [--report report.json]`**
  - **ماذا يفعل:** يمرّر الفيديو عبر قراءة التطبيق نفسه كأنه يُلتقط حيًّا:
    - الإطارات بوتيرتها الأصلية، والقراءة تأخذ أحدثها.
    - توقيت التطبيق ومترجمه وعارضه كما هي.
  - **الخيارات:**
    - `--fps 30`
    - `--rate 1.0`
    - `--tail 5`: ثوانٍ سوداء بعد النهاية.
    - `--engine <id>`
    - `--min-visible 300`
  - **البيانات:**
    - يعمل على نسخة عمل من بيانات التطبيق وذاكرته؛ الأصل يُقرأ فقط.
    - يمكن تحديد نسخة مجمّدة بـ `--data <folder> --db <folder>`، حتى تكون نتائج التشغيلات قابلة للمقارنة.
    - يستعمل التنزيلات المحفوظة، ولا ينزّل شيئًا.
  - **المخرجات:** `report.json` و`report.events.jsonl`.
- **`--evaluate <events.jsonl> --gold gold.json [--report out.json] [--start <Unix ms>]`**
  - يقيس سجلًا سابقًا، كسجل تشغيل أو `lines.log` من جلسة لعب مسجّلة.
  - مع `lines.log` يُعطى `--start`: لحظة بداية الفيديو.
- **`--gold-draft <video> --out draft.json [--fps 10]`**
  - مسودة للمرجع الذهبي: كل سطر يُقرأ في المناطق المفعّلة، بما فيها الأسطر القصيرة، مع زمني ظهوره واختفائه.
  - تُراجع يدويًا: يُصحح النص والأزمنة، ويُحذف ما ليس من اللعبة، ويُضاف ما فات.

**صيغة المرجع الذهبي:** قائمة JSON فيها `{"id", "area", "start_ms", "end_ms", "text", "short"?}`، والأزمنة بزمن الفيديو.

**قواعد المطابقة:**
- يُعدّ السطر مطابقًا إذا تشابه النص بعد التطبيع بنسبة ≥ 0.85.
- كلماته تُطابق كاملة، فكلمة "No" لا تُطابق "now".
- جزء من كتلة متراكمة يُطابق أيضًا.
- النافذة الزمنية من 200ms قبل الظهور إلى 2.5s بعد الاختفاء.

**المقاييس:**
- **D:** نسبة الاكتشاف.
- **T:** نسبة الترجمة.
- **S:** نسبة العرض لمدة ≥ `--min-visible`، مقسومة إلى:
  - العرض في الوقت: والإنجليزية ما زالت ظاهرة.
  - العرض المتأخر.
- **زمن البدء p50 وp90:** من ظهور الإنجليزية إلى ظهور العربية.
- **انتهاكات الترتيب:** عرض سطر أقدم بعد أحدث منه في المنطقة نفسها.
- **العروض الخاطئة:** ترجمة لا يقابلها سطر في المرجع.
- **CER:** نسبة خطأ الأحرف في القراءة.
- **سبب فقد كل سطر.**

كلها مقسومة حسب المنطقة، وحسب الأسطر القصيرة والطويلة.

**أسباب الفقد:**

| السبب | المعنى |
|---|---|
| `not_seen` | لم يُقرأ |
| `not_settled` | قُرئ ولم يستقر |
| `filtered:<reason>` | تركه أحد المرشّحات |
| `skipped:superseded` | تُخطّي لأن سطرًا أحدث سبقه |
| `translate:<kind>` | خطأ في المترجم، أو ترجمة مرفوضة |
| `translate:pending` | أُرسل ولم تصل ترجمته حتى النهاية |
| `presenter:replaced_before_show` | استُبدل قبل أن يُعرض |
| `presenter:not_shown` | تُرجم ولم يُعرض، دون سبب مسجّل |
| `presenter:cleared` | مُسح مع كل المعروض |
| `presenter:late_hidden` | وصل بعد غياب الإنجليزية فأُخفي فورًا |
| `presenter:too_short` | عُرض مدة أقصر من الحد |

## تكافؤ الذاكرة

**على حاسوبك:** من مخرجات بناء النسخة (`vi-translator-xbox-measurement-tools`)، شغّل `memory-check.cmd` والتطبيق مغلق.

**ماذا يفعل:**
- يحمّل ذاكرتك وتنزيلاتها المحفوظة بنواة الأصل وبنواة النسخة.
- يقارن كل إجابة: الترجمة ومصدرها عبر `get`، و`line_of`. يشمل ذلك:
  - كل سطر في الذاكرة والحزمة.
  - قراءات خاطئة لعينة منها.
  - ما يبقى في ملف الذاكرة بعد التنزيل.
- النتيجة إحدى اثنتين: `IDENTICAL` (متطابقة) أو `DIFFERENT` (مختلفة).

**الخصوصية:** مجلداتك تُقرأ فقط، فالأداة تعمل على نسخة مؤقتة. والترجمات تُكتب بصمات SHA-256 فقط، فلا تخرج أي ترجمة مفكوكة.

للمطوّر:

```
git worktree add ../vit-base "$(cat .fork/base-commit)"
mkdir -p ../vit-base/core/examples && cp core/examples/fork_memory_equivalence.rs ../vit-base/core/examples/
cargo run -p vi-translator-core --example fork_memory_equivalence -- sample /tmp/memeq
(cd ../vit-base && cargo run -p vi-translator-core --example fork_memory_equivalence -- dump --db /tmp/memeq/db --pack-jsonl /tmp/memeq/pack.jsonl --out /tmp/memeq/original.jsonl)
cargo run -p vi-translator-core --example fork_memory_equivalence -- dump --db /tmp/memeq/db --pack-jsonl /tmp/memeq/pack.jsonl --out /tmp/memeq/fork.jsonl
cargo run -p vi-translator-core --example fork_memory_equivalence -- compare /tmp/memeq/original.jsonl /tmp/memeq/fork.jsonl
```

## سير العمل (GitHub Actions)

| سير العمل | متى | ماذا |
|---|---|---|
| **Fork guard** | كل دفع وطلب دمج | الحارس، واختبارات النواة، وتكافؤ الذاكرة على بيانات اصطناعية (نواة الأصل مقابل نواة النسخة) |
| **Fork build (Windows x64)** | يدويًا: Actions ← Run workflow | يشغّل الحارس ثم الاختبارات، ثم يبني:<br>• الملف التنفيذي غير الموقّع ومعه SHA-256.<br>• أدوات القياس، ومنها `memory-check.cmd`. |
| **Upstream sync** | أسبوعيًا (الاثنين 03:17 UTC) أو يدويًا | يدمج تحديثات الأصل في الفرع `sync/auto` ويحدّث `.fork/base-commit`، ثم يشغّل الحارس واختبارات Windows، ثم يفتح طلب دمج تراجعه وتدمجه يدويًا.<br>وتُفتح Issue بالوسم `upstream-sync` في هذه الحالات:<br>• تعارض مع تعديلات النسخة.<br>• فشل الحارس.<br>• فشل الاختبارات.<br>• تغيير الأصل لملفات سير العمل. |

**ما يلزم لعمل سير العمل:**
- تفعيل Actions في المستودع (تبويب Actions).
- وجود ملفات سير العمل في الفرع الافتراضي، لأن المجدول والتشغيل اليدوي يعملان منه.
- اختياري:
  - إيقاف سير عمل الأصل الذي لا تحتاجه النسخة: «Phone core» (مكتبات الهاتف، على macOS) و«Release»، من صفحة كل منهما في تبويب Actions ← Disable workflow. هذا لا يعدّل ملفاتهما.
  - تفعيل Settings ← Actions ← General ← «Allow GitHub Actions to create and approve pull requests»، حتى تفتح المزامنة طلب الدمج بنفسها. بدونه تفتح Issue فيها رابط الفرع.
  - إضافة السر `SYNC_TOKEN`: رمز بصلاحية كتابة على محتوى المستودع وطلبات الدمج وسير العمل. لا يلزم إلا إذا غيّر الأصل ملفات سير عمله.

## المزامنة يدويًا (عند التعارض)

```
git remote add upstream https://github.com/vi-translator/vi-translator-desktop.git
git fetch upstream main
git checkout -b sync/manual            # من الفرع الافتراضي للنسخة
git merge upstream/main
# في التعارضات: خطافات النسخة (المعلّمة بـ // fork:) تبقى، وكل ما عداها من الأصل
git rev-parse upstream/main > .fork/base-commit
bash .fork/guard.sh
cargo test -p vi-translator-core
```
