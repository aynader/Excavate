# Privacy

Everything Excavate reads stays on your PC. Nothing is uploaded, and every model runs locally. Excavate goes online
only to download the models you choose, Everything if you ask it to install it, and, with **Update automatically** on
(Settings → About), to check once a day for a new version, which is used only if its signature checks out.

## What counts as sensitive

As it reads a file or an image, Excavate looks for:

- **Card numbers** (checked with the Luhn formula), **IBANs** (checked with their mod-97 digits; beside or under an
  IBAN's or an account's label, such as IBAN, Account number or Kontonummer, any value of an IBAN's shape and length,
  since a made-up one fails the check and so does a real one OCR misread), **ID and passport
  numbers**, including the machine-readable lines on passports (read leniently, as OCR misreads them);
- **Credentials**: API keys and tokens by their known beginnings (`AKIA…`, `ghp_…`, `xox…`, `sk-…`, JSON web tokens),
  private-key blocks (even cut off), bearer tokens, the values of variables named `…_KEY`, `…_SECRET`, `…_TOKEN`, and
  values next to or under labels such as Password, PIN, CVV, Recovery code or API key (`apiKey`, `"client_secret"`),
  in a dozen languages;
- **Values by where they are on screen**: in a picture, the value beside or under its label (a settings dialog, a
  password manager's fields), or down a table's Password column;
- **Codes**: one-time codes in a message about signing in ("482913 is your login code"), codes under their label
  (Verification code, One-time password), backup codes, 2FA setup keys, `otpauth://` links, a booking's reference,
  and a card's CVV by its signature strip;
- **Said aloud**: "my PIN is 4821", "the password is …" in transcripts;
- **Unlabelled tokens**: long, random-looking strings within two lines of a key, token, secret or password word;
- **QR codes that are secrets**: in an image about a Wi-Fi network (its password), about an authenticator or two-factor
  sign-in (the 2FA secret), or about a wallet or a payment;
- **Recovery phrases**: 12 or 24 words in a row from the BIP-39 list;
- **Passports, ID cards, driving licences and bank cards by how they look**: an image the "what images show" model
  (SigLIP 2 on Quick, EmbeddingGemma 2 on Balanced) sees as one, even where none of its text could be read.

## What happens to them

- **In the index**, the secret is replaced by a label before anything is stored. "Wi-Fi password" still finds the
  screenshot, because the word "password" is there; the password itself isn't.
- **In previews**, the secret itself is pixelated in an image (not its whole line), a QR code that is one as a whole,
  a passport, ID card or bank card that looks like one all over, and secrets in text are masked. **Reveal** asks Windows Hello first, and what was revealed is hidden again the next
  time the popup opens.
- **In the results**, an image's or a video's small thumbnail is pixelated whole when it has a secret.
- **Until it's read**, an image Excavate will read is pixelated whole, in the preview, enlarged and in the results,
  with "Not read yet" where the preview says what's hidden (Reveal works the same). Once it's read, the preview shows
  it with only its secrets covered, without having to open it again. A video whose text is read (Settings → Videos,
  Read text in videos) is covered the same way until its frames are read. An image that won't be read shows as it is:
  outside the folders Excavate reads, left out, or in a folder whose text in images is turned off. So does one
  Excavate couldn't read. One nearly the same as another, set aside until the rest are read, stays covered until it's
  read too. The switch that blurs what looks sensitive (Settings → Privacy) turns this off too.
- **While you present** (a slide show, or Windows' presentation mode), previews show no images or text. Settings →
  Privacy turns this off.
- **Office files** (Word, Excel, PowerPoint) are previewed by Office's own previewer only when nothing secret was found
  in them. A file with a secret keeps its text preview, with the secret masked.
- **Found and hidden**, in Settings → Privacy, counts what was found by kind. Seeing where each one is asks Windows
  Hello first; the files are listed with each secret still covered until you choose **Show**, and the list closes
  itself after two minutes. A file marked as holding nothing secret isn't blurred again.

## What else stays on your PC

- **Recent and pinned searches** are kept in Excavate's settings, on your PC, and nowhere else. Settings → Privacy
  clears the recent ones.
- **Excavate Bedrock**, the optional service that reads your drives' file tables, passes Excavate only names, sizes,
  dates and attributes, never what's inside a file, and leaves out other accounts' files on the PC.

## How well it works

Measured on synthetic screenshots and photos made in code (16 kinds with a secret, 7 without; light
and dark, several fonts and sizes, some tilted, re-saved as JPEG, photographed or stored sideways), read by the image
reader Excavate uses on Balanced (PP-OCRv6 small), exactly as the indexer reads them. A secret counts as hidden when
at least 90% of it is under the blur. The rules were tuned on one set; the figures below are from another set, made
with another seed, that they weren't tuned on.

| Kind | Secrets | Hidden before | Hidden now |
|---|---:|---:|---:|
| Wi-Fi cards | 15 | 100% | 100% |
| Router stickers | 14 | 86% | 86% |
| Password managers | 16 | 44% | 94% |
| Wi-Fi settings | 10 | 50% | 100% |
| Saved-password pages | 10 | 0% | 100% |
| .env and config files | 40 | 72% | 98% |
| Terminals with keys | 22 | 68% | 91% |
| Bank cards (front, back) | 10 | 60% | 100% |
| Boarding passes | 10 | 0% | 100% |
| Chats with codes | 10 | 0% | 80% |
| 2FA setup (QR, key) | 20 | 0% | 95% |
| Recovery phrases | 168 | 100% | 100% |
| Backup codes | 80 | 0% | 95% |
| IBANs | 10 | 90% | 90% |
| Passports (fictional) | 30 | 83% | 100% |
| PIN letters | 10 | 60% | 100% |
| **Mean of the kinds** | | **51%** | **95%** |

| Without a secret | Images | Blurred before | Blurred now |
|---|---:|---:|---:|
| Receipts | 10 | 0 | 0 |
| Invoices | 10 | 0 | 0 |
| Notes with numbers | 10 | 0 | 0 |
| Sign-in screens | 10 | 2 | 0 |
| Code | 10 | 10 | 1 |
| A menu's QR code | 10 | 0 | 0 |
| Shipping labels | 10 | 0 | 0 |
| **All** | 70 | **12** | **1** |

By how the image was made, hidden before and now: photographed 44% → 95%, stored sideways (EXIF) 39% → 96%, clean
screenshots 60% → 97%, re-saved as JPEG 73% → 97%, tilted 86% → 100%.

What isn't a secret: 395 real photos with text (TextVQA) had nothing blurred before and one now (the words under a sign
reading "Pin"). Of 1,757 public documents' texts (GovDocs1), 45 had something masked before and 47 now. Typed and
spoken texts (config files, messages with codes, setup screens, transcripts, and as many
without a secret; the rules were tuned on these): 27% of their secrets masked before, all of them now; texts without
one with something masked, 17 of 90 before (code read as passwords), none now.
Looking for secrets takes about 1.8 times as long as before (93 MB of text: 80 s before, 145 s now).

### IBANs under their label

An IBAN was hidden only when its check digits passed. A made-up one fails them (a form's example, a mock-up, a test
account), and so does a real one that OCR misread by one character. Now a value of an IBAN's shape beside or under an
IBAN's or an account's label is hidden either way: its country's length, give or take one character, or for a country
the IBAN registry doesn't list, any IBAN length under a label that says IBAN. Without a label the check digits still
decide, and only for a country that has IBANs (a survey's table of station codes, "AU3395 0086 IDLEWILD", passed them by
chance).

Measured on Balanced, on a new set of the benchmark's images (another seed) with two new kinds: made-up or misread IBANs
under their label, and references that look like IBANs without one.

| | Before | Now |
|---|---:|---:|
| Made-up or misread IBANs under their label, images (10) | 0% | 100% |
| The same in typed and spoken texts (60) | 0% | 100% |
| The other 16 kinds with a secret (images, mean) | 95% | 95% |
| Images without a secret blurred, of 80 (lookalikes included) | 2 | 2 |
| Texts without a secret masked, of 240 (60 IBAN lookalikes included) | 2 | 2 |
| TextVQA photos with anything blurred, of 400 | 0 | 0 |
| GovDocs1 texts with anything masked, of 1,269 (threads 000 and 002) | 44 | 43 |

The two images and two texts without a secret that are blurred are the same before and now: numbers that pass the Luhn
formula inside a parcel's or an invoice's number, and a serial number OCR misread into one whose check digits pass. The
GovDocs1 text that is no longer masked had 15 of those station codes. Quick wasn't measured again (its image reader wasn't on this PC).

### Quick

The same images read by Quick's image reader (PP-OCRv6 tiny), on a third set neither tier's rules were tuned on. Its
misses are misread letters ("Passwond", "Bookingref.", `^` for a passport line's `<`, "CVV485") and lines it doesn't
find in photos of screens; the rules now read the common misreadings too. Neither a larger picture nor lower detection
thresholds found more. The rules cost nothing measurable: 170 ms an image before and after (Balanced: 363 ms).

| Kind | Secrets | Balanced before | Balanced now | Quick before | Quick now |
|---|---:|---:|---:|---:|---:|
| Wi-Fi cards | 17 | 100% | 100% | 100% | 100% |
| Router stickers | 18 | 94% | 94% | 94% | 94% |
| Password managers | 16 | 100% | 100% | 94% | 94% |
| Wi-Fi settings | 10 | 100% | 100% | 90% | 90% |
| Saved-password pages | 10 | 100% | 100% | 100% | 100% |
| .env and config files | 40 | 90% | 90% | 85% | 85% |
| Terminals with keys | 23 | 100% | 100% | 87% | 87% |
| Bank cards (front, back) | 10 | 80% | 100% | 80% | 100% |
| Boarding passes | 10 | 90% | 90% | 80% | 80% |
| Chats with codes | 10 | 90% | 90% | 90% | 90% |
| 2FA setup (QR, key) | 20 | 100% | 100% | 100% | 100% |
| Recovery phrases | 168 | 100% | 100% | 100% | 100% |
| Backup codes | 80 | 94% | 94% | 92% | 92% |
| IBANs | 10 | 100% | 100% | 100% | 100% |
| Passports (fictional) | 30 | 100% | 100% | 97% | 97% |
| PIN letters | 10 | 90% | 90% | 90% | 90% |
| **Mean of the kinds** | | **96%** | **97%** | **92%** | **94%** |

On the two sets the misreadings were found on, Quick went from 93% to 96% and from 94% to 95%. Photographed screens are
where Quick falls behind (85% hidden, Balanced 92%); clean screenshots are 100% on both. Without a secret, Quick blurred
something in 3 of 70 images (a sign-in screen, two of code), Balanced in none; of 395 TextVQA photos, neither blurred
any.

### Whole images

Whether an image looks like a passport, an ID card, a driving licence or a bank card is told from the vector Excavate
keeps for searching images by what they show: how much closer it is to "a passport", "an ID card", "a driver's
license", "a bank card" (seven descriptions) than to the nearest of 23 ordinary ones ("a photo", "a receipt", "a license
plate", "a photo of objects on a table"…). Nothing more is computed: the descriptions' vectors are made once and ship
with Excavate.

Measured on 184 public specimen documents (governments' passport, ID and licence specimens and published data pages on
Wikimedia Commons), the benchmark's 40 bank cards and passports, and 8,166
ordinary photos (COCO and TextVQA). The threshold was chosen on half of them and checked on the other half, both ways
round:

| | SigLIP 2 (Quick) | EmbeddingGemma 2 (Balanced) |
|---|---:|---:|
| Documents and cards blurred whole, other half | 96% and 97% | 95% and 98% |
| Ordinary photos blurred, per 1,000, other half | 0.5 and 1.2 | 0.5 and 2.4 |
| With the threshold Excavate uses (all photos): documents and cards | 96% | 96% |
| … ordinary photos, per 1,000 | 1 | 1 |
| Specimens alone | 98% of 184 | 97% of 184 |
| The benchmark's bank cards | 15 of 20 | 15 of 20 |
| Receipts, invoices, notes, code, sign-in screens (140) | none | none |

The ordinary photos it blurs are near the subject, or about travel and money: a passport on a laptop, a name badge, a
mug printed "iD", a library's "ID required" notice, coins, a bank's logo, a card reader, an airport's departure board,
luggage. On Thorough, an image either of its models sees as one is blurred (Qwen3-VL isn't measured, so only
EmbeddingGemma 2 judges beside it).

Not yet measured: real photos of real cards and documents (the specimens are flat scans), and signed documents or
password managers as whole images (a password manager's screen scored far above ordinary photos, but there was nothing
ordinary of its kind to check it against).

## What it can't promise

Detection isn't perfect: a password written without a label and not shaped like one, in a script the image reader
can't read (Cyrillic, Arabic: Deep reads those, and a secret only it finds blurs the whole picture), or misread in a
blurry photo can get through. An image in a folder whose text in images is turned off is never read, so it shows as
it is. What it reads still stays on your PC either way.

The index is in `%LOCALAPPDATA%\Excavate`. Deleting that folder deletes everything Excavate has read.
