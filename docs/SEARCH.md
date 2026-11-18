# Searching

Type a few letters and Excavate searches names, the words inside files and what images show, all at once. That's
enough most of the time. This page is for the rest: Everything's search syntax, which Excavate reads too, and the
functions it adds. (A test checks this page against the code, so every function the search knows is listed here.)

## Words

| Type | Finds |
|---|---|
| `budget 2024` | names with both words, in any order (a space is AND) |
| `invoice\|receipt` | either word (`\|` is OR; it binds tighter than a space: `a b\|c` is a and (b or c)) |
| `report !draft` | names with "report" but not "draft" (`!` is NOT) |
| `<photo\|image> !thumb` | `< >` group |
| `"annual report"` | the words together, as written (quotes also keep `\|` and `!` as plain characters) |
| `*.iso` `IMG_????.jpg` | `*` any characters, `?` one: matches the whole name |
| `projects\notes` | with a `\`, the full path is searched |
| `"C:\Users\Me\Documents\" tax` | a folder in quotes ending in `\`: only inside it |
| `vsc` | initials (Visual Studio Code); `mikro11` finds `Micro11.iso` with **Fuzzy match** (the menu at the field's end) |

Case and accents are ignored unless asked for. `quot:` and `apos:` stand for `"` and `'`, `#<n>:` for the character
with that code.

## Modifiers

Put one in front of a word (`case:Report`), or on its own to keep only files or folders (`file: ext:pdf`). They can
be combined (`case:ww:Log`).

| Modifier | Means |
|---|---|
| `case:` / `nocase:` | match case (<kbd>Ctrl</kbd>+<kbd>I</kbd>) |
| `ww:` / `noww:` (`wholeword:` `wholewords:` `nowholeword:` `nowholewords:`) | whole words only (<kbd>Ctrl</kbd>+<kbd>B</kbd>) |
| `path:` / `nopath:` | search the full path (<kbd>Ctrl</kbd>+<kbd>U</kbd>) |
| `file:` `files:` / `folder:` `folders:` | only files / only folders |
| `diacritics:` / `nodiacritics:` | accents count (`é` isn't `e`) |
| `ascii:` `utf8:` `noregex:` | accepted, as Everything does; they change nothing here |

## Functions

A function is a name, a colon and a value, with no space after the colon: `ext:pdf`. Ranges take `>` `>=` `<` `<=`
`=` in front, or `..` between two values (`size:1mb..10mb`, `dm:2024-01..2024-03`).

**Names and places**

| Function | Example | Finds |
|---|---|---|
| `ext:` | `ext:pdf;docx` | these extensions (`;` between them) |
| `regex:` | `regex:^IMG_\d+\.jpg$` | names matching a regular expression (<kbd>Ctrl</kbd>+<kbd>R</kbd> makes the whole search one) |
| `startwith:` `endwith:` | `startwith:invoice` | names starting or ending so |
| `wfn:` `wholefilename:` `exact:` | `wfn:readme.md` | exactly this name |
| `len:` | `len:>60` | names of this many characters |
| `parent:` `infolder:` `nosubfolders:` | `parent:C:\Users\Me\Downloads` | directly in that folder, not in its subfolders |
| `shell:` | `shell:Downloads` | inside a known folder (Desktop, Documents, Downloads, Pictures, Music, Videos…) |
| `depth:` `parents:` | `depth:<=2` | this many folders deep |
| `root:` | `root:` | at the top of a drive |
| `filelist:` | `filelist:a.txt\|b.txt` | exactly these names (`\|` between them) |
| `type:` | `type:image` | files whose type, as Windows names it ("JPEG image"), has these words |

**Kinds** (Everything's lists of extensions; a value narrows them: `pic:holiday`)

`audio:` `doc:` `exe:` `pic:` `video:` `zip:`

**Dates and sizes**

| Function | Example | Finds |
|---|---|---|
| `dm:` `datemodified:` | `dm:thisweek` | modified then |
| `dc:` `datecreated:` | `dc:2024` | made then |
| `da:` `dateaccessed:` | `da:yesterday` | last opened then (as Windows records it) |
| `size:` | `size:>100mb` | this big (`kb` `mb` `gb` `tb`; or `empty` `tiny` `small` `medium` `large` `huge` `gigantic`) |
| `attrib:` `attributes:` | `attrib:h` | with these attributes: `r` read-only, `h` hidden, `s` system, `d` folder, `a` archive, `n` normal, `t` temporary, `p` sparse, `l` link, `c` compressed, `o` offline, `i` not indexed, `e` encrypted, `v` integrity |

Dates: `today` `yesterday` `tomorrow` `thisweek` `lastweek` `thismonth` `lastmonth` `thisyear` `lastyear`;
`last3days` (also `hours` `minutes` `weeks` `months` `years`; `past` for `last`); `2024`, `2024-05`, `2024-05-31`
(also with `/` or `.`). The week starts on the day Windows is set to start it.

**Folders**

| Function | Example | Finds |
|---|---|---|
| `childcount:` `childfilecount:` `childfoldercount:` | `childfilecount:>1000` | folders holding this many items, files or folders |
| `empty:` | `empty:` | empty folders |
| `child:` | `child:package.json` | folders with a file or folder of that name directly inside |

**Duplicates**

| Function | Example | Finds |
|---|---|---|
| `dupe:` | `dupe: ext:jpg` | names that occur more than once |
| `sizedupe:` | `sizedupe: size:>1gb` | files whose size occurs more than once |
| `namepartdupe:` `dmdupe:` `dcdupe:` `dadupe:` `attribdupe:` | `namepartdupe:` | the same name without extension, date modified, made, opened, or attributes |

**Order and how many**

| Function | Example | Does |
|---|---|---|
| `sort:` | `sort:size-descending` | orders by `name` `path` `size` `ext` (`type`) `dm` `dc` `da`, `-ascending` or `-descending` after it |
| `ascending:` `descending:` | `ext:mp4 descending:` | the order alone |
| `count:` | `count:20` | no more results than this |

**Answered by Excavate itself** (from what it has read, not from the names)

| Function | Example | Finds |
|---|---|---|
| `content:` `utf8content:` | `content:"quarterly report"` | the words inside: documents, code, text in images and videos, what's said in recordings |
| `width:` `height:` | `width:>3000` | images and videos this many pixels wide or high |
| `dimensions:` | `dimensions:1920x1080` | both at once (`>1920x1080`: at least that on each side) |
| `length:` | `length:>10:00` | recordings and videos this long (`30s`, `5min`, `1:30:00`) |
| `runcount:` `rc:` | `rc:>5` | opened from Excavate this many times |
| `daterun:` `dr:` | `dr:thisweek` | last opened from Excavate then |

Not answered: `orientation:`, `bitdepth:` and `frontmatter:` (Everything's) match nothing, and the footer says so.

## Words that become filters

As you type, words that name a file format or a time become chips in the field, and the rest stays words:
`pdfs from last week invoice` searches PDFs changed last week for "invoice". A chip forms once its words are finished
(a space after them).

| Typed | Chip |
|---|---|
| `pdf` `pdfs` `pdf files` `pdf documents` | PDF (`ext:pdf`) |
| `doc` `docx` `word documents` `word docs` `word files` | Word (`ext:doc;docx`) |
| `xls` `xlsx` `spreadsheets` `excel files` `excel sheets` | Spreadsheets (`ext:xls;xlsx;xlsm;csv;ods`) |
| `ppt` `pptx` `powerpoints` `powerpoint files` | Presentations (`ext:ppt;pptx;odp`) |
| `zips` `zip files` | Archives (`ext:zip;7z;rar`) |
| `mp3` `mp4` `jpg` `jpeg` `png`, and with an `s` | MP3, MP4, JPEG, PNG |
| `today` `yesterday` `this week` `last week` `this month` `last month` `this year` `last year` | Today, Last week and the rest (`dm:`) |

"from", "since", "in", "during", "modified", "changed", "edited", "made", "created" or "saved" in front of a time go
into its chip. Kinds such as "photos" or "invoice" stay words, since they tell a search by meaning what you want.
Quotes keep words as words, and Backspace in an empty search turns the last chip back into words, which then stay
words.

## Bedrock and Everything

Excavate's own index, Bedrock, answers everything on this page. With Everything as the index instead (chosen during
setup), Everything answers the name functions by its own rules, and `dc:`, `da:` and `attrib:` work only with Bedrock.

## `>` and `<` (experimental)

With the switch in Settings → General, a search can start with `>` for the words inside, or `<` for what files show or
mean. A type right after it narrows the search:

| Type | Searches |
|---|---|
| `> invoice` | the text in images |
| `< a dog on a beach` | images by what they show |
| `>v theory of computation` | the words in videos, on screen and said (`>vt` on screen only, `>vs` said only) |
| `<v sunset` | videos by what they show |
| `>a budget` | what's said in recordings |
| `>d` `>c` `>pdf` | the words in documents, in code, in PDFs (any extension works) |
| `<d` `<pdf` | documents, PDFs by what they mean |
