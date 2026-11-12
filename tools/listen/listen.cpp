// excavate-listen: what's said in recordings, for Excavate's indexer. One model stays loaded (Whisper or Parakeet,
// through whisper.cpp), and the indexer hands it sound over stdin, already decoded by Windows (16 kHz mono floats), a
// piece at a time; each answer is one line of JSON on stdout: what was said, with when.
//
//   excavate-listen --list
//       the devices ggml sees, one per line: "<index>\t<backend>\t<name>\t<gpu|igpu|cpu|accel>\t<MB>"
//   excavate-listen --whisper|--parakeet <model> [--threads N] [--backend vulkan|cuda|cpu] [--vad <model>]
//       then, for each piece: "LISTEN <samples> <language|auto> <languages|->\n" and <samples> 32-bit floats; it
//       answers {"language":"en","ms":1234,"speech_ms":56000,"vad_ms":80,"pieces":[{"t0":0,"t1":5400,"text":"..."}, ...]}
//       or {"error":"..."}. <languages>: the person's languages ("en,ar"): with "auto", Whisper chooses among these only.
//       --vad: Silero's voice detector, first, so the model only hears speech (silence and music make Whisper invent
//       words, and cost time): Whisper hears the stretches where someone speaks joined, its moments mapped back;
//       Parakeet hears each run of them as it is. With no speech in a piece, the model isn't run. speech_ms: how much of the piece was speech; vad_ms: the detector's
//       time.
//
// Which Vulkan card it uses is the indexer's choice (GGML_VK_VISIBLE_DEVICES): Vulkan lists a processor's own graphics
// first, and whisper.cpp takes the first.

#include "ggml-backend.h"
#include "parakeet.h"
#include "whisper.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#endif

namespace {

struct Piece {
    int64_t t0;  // ms
    int64_t t1;
    std::string text;
};

void quiet(enum ggml_log_level, const char *, void *) {}

// Only whole UTF-8 characters: Whisper's pieces can end or start partway through one (its tokens are bytes).
std::string whole_characters(const std::string & text) {
    std::string out;
    for (size_t i = 0; i < text.size();) {
        unsigned char c = static_cast<unsigned char>(text[i]);
        size_t length = c < 0x80 ? 1 : (c >> 5) == 0x6 ? 2 : (c >> 4) == 0xE ? 3 : (c >> 3) == 0x1E ? 4 : 0;
        bool whole = length > 0 && i + length <= text.size();
        for (size_t k = 1; whole && k < length; ++k) whole = (static_cast<unsigned char>(text[i + k]) >> 6) == 0x2;
        if (whole) out.append(text, i, length);
        i += whole ? length : 1;
    }
    return out;
}

std::string escaped(const std::string & raw) {
    std::string out;
    std::string text = whole_characters(raw);
    for (unsigned char c : text) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (c < 0x20) {
                    char buffer[8];
                    snprintf(buffer, sizeof buffer, "\\u%04x", c);
                    out += buffer;
                } else {
                    out += static_cast<char>(c);
                }
        }
    }
    return out;
}

std::string trimmed(const std::string & text) {
    size_t start = text.find_first_not_of(" \t\r\n");
    if (start == std::string::npos) return "";
    size_t end = text.find_last_not_of(" \t\r\n");
    return text.substr(start, end - start + 1);
}

void answer(const std::string & language, long long ms, long long speech_ms, long long vad_ms, const std::vector<Piece> & pieces) {
    std::string out = "{\"language\":\"" + escaped(language) + "\",\"ms\":" + std::to_string(ms) +
                      (speech_ms >= 0 ? ",\"speech_ms\":" + std::to_string(speech_ms) + ",\"vad_ms\":" + std::to_string(vad_ms) : "") +
                      ",\"pieces\":[";
    bool first = true;
    for (const Piece & piece : pieces) {
        std::string text = trimmed(piece.text);
        if (text.empty()) continue;
        if (!first) out += ",";
        first = false;
        out += "{\"t0\":" + std::to_string(piece.t0) + ",\"t1\":" + std::to_string(piece.t1) + ",\"text\":\"" + escaped(text) + "\"}";
    }
    out += "]}\n";
    fputs(out.c_str(), stdout);
    fflush(stdout);
}

void fail(const std::string & error) {
    fputs(("{\"error\":\"" + escaped(error) + "\"}\n").c_str(), stdout);
    fflush(stdout);
}

const char * kind(enum ggml_backend_dev_type type) {
    switch (type) {
        case GGML_BACKEND_DEVICE_TYPE_GPU: return "gpu";
        case GGML_BACKEND_DEVICE_TYPE_IGPU: return "igpu";
        case GGML_BACKEND_DEVICE_TYPE_CPU: return "cpu";
        default: return "accel";
    }
}

// The device whisper.cpp should take, counted as it counts them (graphics devices only, in order): the first dedicated
// card of the backend asked for. -1: none, so the processor.
int chosen_device(const std::string & backend) {
    if (backend == "cpu") return -1;
    int counted = 0;
    for (size_t i = 0; i < ggml_backend_dev_count(); ++i) {
        ggml_backend_dev_t device = ggml_backend_dev_get(i);
        enum ggml_backend_dev_type type = ggml_backend_dev_type(device);
        if (type != GGML_BACKEND_DEVICE_TYPE_GPU && type != GGML_BACKEND_DEVICE_TYPE_IGPU) continue;
        std::string registry = ggml_backend_reg_name(ggml_backend_dev_backend_reg(device));
        bool wanted = backend.empty() || _strnicmp(registry.c_str(), backend.c_str(), backend.size()) == 0;
        if (wanted && type == GGML_BACKEND_DEVICE_TYPE_GPU) return counted;
        ++counted;
    }
    return -1;
}

bool read_exactly(void * into, size_t bytes) {
    char * at = static_cast<char *>(into);
    while (bytes > 0) {
        size_t got = fread(at, 1, bytes, stdin);
        if (got == 0) return false;
        at += got;
        bytes -= got;
    }
    return true;
}

// Whisper's language: the one asked for, or, when it's left to tell, the likeliest of the person's languages (all of
// them when none are given).
std::string whisper_language(whisper_context * context, const std::vector<float> & samples, const std::string & asked,
                             const std::string & languages, int threads) {
    if (asked != "auto") return asked;
    if (whisper_pcm_to_mel(context, samples.data(), static_cast<int>(std::min<size_t>(samples.size(), 16000 * 30)), threads) != 0) return "auto";
    std::vector<float> probabilities(whisper_lang_max_id() + 1, 0.0f);
    int best = whisper_lang_auto_detect(context, 0, threads, probabilities.data());
    if (best < 0) return "auto";
    // Sure of another language: a recording in it, whatever the person's own are.
    if (languages == "-" || languages.empty() || probabilities[best] > 0.8f) return whisper_lang_str(best);
    std::string choice;
    float most = -1.0f;
    size_t start = 0;
    while (start <= languages.size()) {
        size_t comma = languages.find(',', start);
        std::string code = languages.substr(start, comma == std::string::npos ? std::string::npos : comma - start);
        int id = whisper_lang_id(code.c_str());
        if (id >= 0 && probabilities[id] > most) {
            most = probabilities[id];
            choice = code;
        }
        if (comma == std::string::npos) break;
        start = comma + 1;
    }
    return choice.empty() ? whisper_lang_str(best) : choice;
}

std::vector<Piece> whisper_pieces(whisper_context * context) {
    std::vector<Piece> pieces;
    int count = whisper_full_n_segments(context);
    for (int i = 0; i < count; ++i) {
        // (Whisper's times are in hundredths of a second.)
        pieces.push_back({whisper_full_get_segment_t0(context, i) * 10, whisper_full_get_segment_t1(context, i) * 10,
                          whisper_full_get_segment_text(context, i)});
    }
    return pieces;
}

// A stretch of speech: where it is in the sound handed over ([from, to), samples) and where it starts in the speech
// alone.
struct Stretch {
    size_t from, to, at;
};

// The speech alone, as Silero's voice detector finds it: its stretches, padded, one after another with a tenth of a
// second of quiet between them. Silence, music and noise are left out, so they cost nothing and nothing's invented for
// them. Empty: no one speaks in it.
std::vector<Stretch> speech_only(whisper_vad_context * detector, const std::vector<float> & samples, std::vector<float> & speech) {
    std::vector<Stretch> stretches;
    speech.clear();
    whisper_vad_params params = whisper_vad_default_params();
    params.speech_pad_ms = 100;
    params.min_silence_duration_ms = 200;
    // (For measuring other choices.)
    if (const char * pad = getenv("EXCAVATE_LISTEN_VAD_PAD")) params.speech_pad_ms = atoi(pad);
    if (const char * silence = getenv("EXCAVATE_LISTEN_VAD_SILENCE")) params.min_silence_duration_ms = atoi(silence);
    whisper_vad_segments * found = whisper_vad_segments_from_samples(detector, params, samples.data(), static_cast<int>(samples.size()));
    if (found == nullptr) return stretches;
    const size_t gap = 1600;
    for (int i = 0; i < whisper_vad_segments_n_segments(found); ++i) {
        // (In hundredths of a second.)
        size_t from = static_cast<size_t>(std::max(0.0f, whisper_vad_segments_get_segment_t0(found, i)) * 160.0f);
        size_t to = std::min(samples.size(), static_cast<size_t>(whisper_vad_segments_get_segment_t1(found, i) * 160.0f));
        if (!stretches.empty() && from < stretches.back().to + gap) {
            // Close to the last one: one stretch, the quiet between kept as it was.
            size_t from_last = stretches.back().to;
            if (to > from_last) {
                speech.insert(speech.end(), samples.begin() + from_last, samples.begin() + to);
                stretches.back().to = to;
            }
            continue;
        }
        if (to <= from) continue;
        if (!stretches.empty()) speech.insert(speech.end(), gap, 0.0f);
        stretches.push_back({from, to, speech.size()});
        speech.insert(speech.end(), samples.begin() + from, samples.begin() + to);
    }
    whisper_vad_free_segments(found);
    return stretches;
}

// A moment in the speech alone (ms), as a moment in the sound handed over.
int64_t original_ms(const std::vector<Stretch> & stretches, int64_t ms) {
    if (stretches.empty()) return ms;
    int64_t at = ms * 16;
    const Stretch * in = &stretches.front();
    for (const Stretch & stretch : stretches) {
        if (static_cast<int64_t>(stretch.at) > at) break;
        in = &stretch;
    }
    int64_t into = std::clamp<int64_t>(at - static_cast<int64_t>(in->at), 0, static_cast<int64_t>(in->to - in->from));
    return (static_cast<int64_t>(in->from) + into) / 16;
}

// Where to cut sound for Parakeet: the quietest tenth of a second between 20 and 30 seconds in (its attention costs
// grow with the square of what it's given: five minutes at once ran at a third of its speed on short ones).
size_t parakeet_cut(const std::vector<float> & samples, size_t start, size_t stop) {
    const size_t rate = 16000, frame = rate / 10, soonest = 20 * rate, latest = 30 * rate;
    if (stop - start <= latest) return stop;
    size_t best = start + latest;
    double quietest = 1e30;
    for (size_t at = start + soonest; at + frame <= start + latest; at += frame / 2) {
        double energy = 0;
        for (size_t i = at; i < at + frame; ++i) energy += static_cast<double>(samples[i]) * samples[i];
        if (energy < quietest) {
            quietest = energy;
            best = at + frame / 2;
        }
    }
    return best;
}

// Parakeet's words, grouped into pieces a few seconds long: a new one after a pause, after a sentence once the piece
// has some length, or once it's long.
std::vector<Piece> parakeet_pieces(parakeet_context * context) {
    std::vector<Piece> pieces;
    Piece piece{-1, 0, ""};
    int count = parakeet_full_n_segments(context);
    char text[256];
    for (int segment = 0; segment < count; ++segment) {
        int tokens = parakeet_full_n_tokens(context, segment);
        for (int i = 0; i < tokens; ++i) {
            parakeet_token_data token = parakeet_full_get_token_data(context, segment, i);
            const char * raw = parakeet_token_to_str(context, token.id);
            if (raw == nullptr) continue;
            int64_t t0 = token.t0 * 10, t1 = token.t1 * 10;
            bool ended = !piece.text.empty() && strchr(".?!", piece.text.back()) != nullptr;
            bool split = piece.t0 >= 0 && token.is_word_start &&
                         (t0 - piece.t1 > 800 || (ended && piece.t1 - piece.t0 > 3000) || piece.t1 - piece.t0 > 12000);
            if (split) {
                pieces.push_back(piece);
                piece = {-1, 0, ""};
            }
            parakeet_token_to_text(raw, piece.text.empty(), text, sizeof text);
            if (piece.t0 < 0) piece.t0 = t0;
            piece.t1 = std::max(piece.t1, t1);
            piece.text += text;
        }
    }
    if (piece.t0 >= 0) pieces.push_back(piece);
    return pieces;
}

}  // namespace

int main(int argc, char ** argv) {
#ifdef _WIN32
    _setmode(_fileno(stdin), _O_BINARY);
#endif
    std::string model, backend, mode, vad;
    int threads = static_cast<int>(std::min(8u, std::max(1u, std::thread::hardware_concurrency() / 2)));
    bool list = false;
    for (int i = 1; i < argc; ++i) {
        std::string arg = argv[i];
        if (arg == "--list") list = true;
        else if ((arg == "--whisper" || arg == "--parakeet") && i + 1 < argc) { mode = arg.substr(2); model = argv[++i]; }
        else if (arg == "--threads" && i + 1 < argc) threads = std::max(1, atoi(argv[++i]));
        else if (arg == "--backend" && i + 1 < argc) backend = argv[++i];
        else if (arg == "--vad" && i + 1 < argc) vad = argv[++i];
        else { fprintf(stderr, "unknown argument: %s\n", arg.c_str()); return 2; }
    }
    if (getenv("EXCAVATE_LISTEN_LOG") == nullptr) {
        whisper_log_set(quiet, nullptr);
        parakeet_log_set(quiet, nullptr);
        ggml_log_set(quiet, nullptr);
    }
    ggml_backend_load_all();
    if (list) {
        for (size_t i = 0; i < ggml_backend_dev_count(); ++i) {
            ggml_backend_dev_t device = ggml_backend_dev_get(i);
            size_t free = 0, total = 0;
            ggml_backend_dev_memory(device, &free, &total);
            printf("%zu\t%s\t%s\t%s\t%zu\n", i, ggml_backend_reg_name(ggml_backend_dev_backend_reg(device)),
                   ggml_backend_dev_description(device), kind(ggml_backend_dev_type(device)), total >> 20);
        }
        return 0;
    }
    if (model.empty()) {
        fprintf(stderr, "usage: excavate-listen --list | --whisper|--parakeet <model> [--threads N] [--backend vulkan|cuda|cpu]\n");
        return 2;
    }
    int device = chosen_device(backend);
    whisper_context * whisper = nullptr;
    parakeet_context * parakeet = nullptr;
    if (mode == "whisper") {
        whisper_context_params params = whisper_context_default_params();
        params.use_gpu = device >= 0;
        params.gpu_device = std::max(device, 0);
        params.flash_attn = true;
        whisper = whisper_init_from_file_with_params(model.c_str(), params);
    } else {
        parakeet_context_params params = parakeet_context_default_params();
        params.use_gpu = device >= 0;
        params.gpu_device = std::max(device, 0);
        parakeet = parakeet_init_from_file_with_params(model.c_str(), params);
    }
    if (whisper == nullptr && parakeet == nullptr) {
        fail("the model didn't load: " + model);
        return 1;
    }
    // The voice detector, once, on the processor (it's small: a minute of sound in about a tenth of a second).
    // EXCAVATE_LISTEN_INNER_VAD: Whisper's own use of it instead (each piece, the way it ran before), for measuring.
    bool inner_vad = getenv("EXCAVATE_LISTEN_INNER_VAD") != nullptr && whisper != nullptr;
    whisper_vad_context * detector = nullptr;
    if (!vad.empty() && !inner_vad) {
        whisper_vad_context_params params = whisper_vad_default_context_params();
        const char * vad_threads = getenv("EXCAVATE_LISTEN_VAD_THREADS");
        params.n_threads = vad_threads != nullptr ? std::max(1, atoi(vad_threads)) : 1;
        params.use_gpu = false;
        detector = whisper_vad_init_from_file_with_params(vad.c_str(), params);
        if (detector == nullptr) {
            fail("the voice detector didn't load: " + vad);
            return 1;
        }
    }
    // Ready: the indexer waits for this line.
    printf("{\"ready\":true,\"device\":%d}\n", device);
    fflush(stdout);
    char line[512];
    std::vector<float> samples, speech;
    while (fgets(line, sizeof line, stdin) != nullptr) {
        long long count = 0;
        char language[32] = "auto", languages[256] = "-";
        if (sscanf(line, "LISTEN %lld %31s %255s", &count, language, languages) < 1 || count < 0 || count > 16000LL * 60 * 60) {
            fail("not a request");
            return 1;
        }
        samples.resize(static_cast<size_t>(count));
        if (count > 0 && !read_exactly(samples.data(), samples.size() * sizeof(float))) return 0;
        auto started = std::chrono::steady_clock::now();
        std::vector<Piece> pieces;
        std::string said = language;
        long long speech_ms = -1, vad_ms = 0;
        // The speech alone, when the detector's loaded; what's heard is then that, and its moments are mapped back.
        std::vector<Stretch> stretches;
        if (detector != nullptr && count >= 4000) {
            stretches = speech_only(detector, samples, speech);
            speech_ms = static_cast<long long>(speech.size()) / 16;
            vad_ms = std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - started).count();
        }
        const std::vector<float> & heard = detector != nullptr ? speech : samples;
        // Under a quarter of a second (of speech): nothing to hear.
        if (heard.size() >= 4000) {
            if (whisper != nullptr) {
                said = whisper_language(whisper, heard, language, languages, threads);
                whisper_full_params params = whisper_full_default_params(WHISPER_SAMPLING_GREEDY);
                params.n_threads = threads;
                params.language = said.c_str();
                params.translate = false;
                params.print_progress = false;
                params.print_realtime = false;
                params.print_timestamps = false;
                params.print_special = false;
                params.no_context = true;
                params.suppress_blank = true;
                params.suppress_nst = true;
                if (inner_vad && !vad.empty()) {
                    params.vad = true;
                    params.vad_model_path = vad.c_str();
                }
                if (whisper_full(whisper, params, heard.data(), static_cast<int>(heard.size())) != 0) {
                    fail("Whisper couldn't listen to it");
                    continue;
                }
                said = whisper_lang_str(whisper_full_lang_id(whisper));
                pieces = whisper_pieces(whisper);
            } else {
                parakeet_full_params params = parakeet_full_default_params(PARAKEET_SAMPLING_GREEDY);
                params.n_threads = threads;
                bool ok = true;
                // The runs of speech as the sound has them, not joined: stretches a second or less apart are heard
                // together, up to 30 s (joined speech made Parakeet lose words over music).
                std::vector<std::pair<size_t, size_t>> runs;
                if (detector != nullptr) {
                    for (const Stretch & stretch : stretches) {
                        if (!runs.empty() && stretch.from <= runs.back().second + 16000 && stretch.to - runs.back().first <= 30 * 16000) {
                            runs.back().second = stretch.to;
                        } else {
                            runs.push_back({stretch.from, stretch.to});
                        }
                    }
                } else {
                    runs.push_back({0, samples.size()});
                }
                for (const auto & [from, to] : runs) {
                    for (size_t start = from; start < to && ok;) {
                        size_t end = parakeet_cut(samples, start, to);
                        // (A last sliver under a quarter of a second has nothing to hear.)
                        if (end - start >= 4000) {
                            ok = parakeet_full(parakeet, params, samples.data() + start, static_cast<int>(end - start)) == 0;
                            int64_t offset = static_cast<int64_t>(start) * 1000 / 16000;
                            for (Piece & piece : parakeet_pieces(parakeet)) {
                                pieces.push_back({piece.t0 + offset, piece.t1 + offset, piece.text});
                            }
                        }
                        start = end;
                    }
                }
                if (!ok) {
                    fail("Parakeet couldn't listen to it");
                    continue;
                }
                said = "";
            }
        }
        // (Whisper heard the speech joined: its moments are mapped back. Parakeet's are the sound's own.)
        if (detector != nullptr && whisper != nullptr) {
            for (Piece & piece : pieces) {
                piece.t0 = original_ms(stretches, piece.t0);
                piece.t1 = std::max(piece.t0, original_ms(stretches, piece.t1));
            }
        }
        long long ms = std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - started).count();
        answer(said, ms, speech_ms, vad_ms, pieces);
    }
    if (detector != nullptr) whisper_vad_free(detector);
    if (whisper != nullptr) whisper_free(whisper);
    if (parakeet != nullptr) parakeet_free(parakeet);
    // (Vulkan's driver can end the process badly at exit; everything's answered by now.)
    fflush(stdout);
    _exit(0);
}
