typedef unsigned long long u64;
typedef unsigned int u32;
typedef unsigned char u8;

extern "C" __device__ u64 fnv1a64(const u8* s, u32 len) {
    u64 h = 0xcbf29ce484222325ULL;
    for (u32 i = 0; i < len; i++) {
        h ^= (u64)s[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

extern "C" __device__ bool probe(const u64* table, u64 mask, u64 hash) {
    // Open addressing, linear probing. 0 = empty slot (names hashing to 0
    // are stored as 1 — collision-safe for a membership filter).
    u64 h = hash == 0 ? 1 : hash;
    u64 slot = h & mask;
    for (u32 i = 0; i < 128; i++) {
        u64 v = table[(slot + i) & mask];
        if (v == h) return true;
        if (v == 0) return false;
    }
    return false;
}

// One thread per reference. Mirrors ReferenceResolver::has_any_possible_match
// EXACTLY: full name; first-'.' receiver/rest/capitalized-receiver plus
// last-'.' tail; first-"::" receiver/rest; last-'/' filename. out[i] = 1 when
// any probe hits, 0 when none can. Bit 0x80 = first receiver byte is
// non-ASCII (capitalize_first semantics diverge) -> caller re-checks on CPU.
extern "C" __global__ void probe_names(
    const u8* __restrict__ buf,
    const u32* __restrict__ offsets, // len = n_refs + 1
    const u64* __restrict__ table,
    u64 mask,
    u8* __restrict__ out,
    u32 n_refs
) {
    u32 i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n_refs) return;
    u32 start = offsets[i], end = offsets[i + 1];
    u32 len = end - start;
    const u8* s = buf + start;

    if (probe(table, mask, fnv1a64(s, len))) { out[i] = 1; return; }
    u8 flags = 0;

    int first_dot = -1, last_dot = -1, first_colon = -1, last_slash = -1;
    for (u32 j = 0; j < len; j++) {
        if (s[j] == '.') { if (first_dot < 0) first_dot = (int)j; last_dot = (int)j; }
        if (first_colon < 0 && j + 1 < len && s[j] == ':' && s[j+1] == ':') first_colon = (int)j;
        if (s[j] == '/') last_slash = (int)j;
    }

    if (first_dot > 0) {
        u32 d = (u32)first_dot;
        if (probe(table, mask, fnv1a64(s, d))) { out[i] = 1; return; }          // receiver
        if (probe(table, mask, fnv1a64(s + d + 1, len - d - 1))) { out[i] = 1; return; } // rest
        u8 c0 = s[0];
        if (c0 >= 'a' && c0 <= 'z') {                                            // capitalized recv
            u64 h = 0xcbf29ce484222325ULL;
            h ^= (u64)(c0 - 32); h *= 0x100000001b3ULL;
            for (u32 j = 1; j < d; j++) { h ^= (u64)s[j]; h *= 0x100000001b3ULL; }
            if (probe(table, mask, h)) { out[i] = 1; return; }
        } else if (c0 >= 0x80) {
            flags |= 0x80; // unicode capitalize — defer to CPU
        }
        if (last_dot > first_dot && (u32)last_dot + 1 < len) {                   // FQN tail
            u32 ld = (u32)last_dot;
            if (probe(table, mask, fnv1a64(s + ld + 1, len - ld - 1))) { out[i] = 1; return; }
        }
    }
    if (first_colon > 0) {
        u32 c = (u32)first_colon;
        if (probe(table, mask, fnv1a64(s, c))) { out[i] = 1; return; }
        if (c + 2 <= len && probe(table, mask, fnv1a64(s + c + 2, len - c - 2))) { out[i] = 1; return; }
    }
    if (last_slash > 0 && (u32)last_slash + 1 < len) {
        u32 sl = (u32)last_slash;
        if (probe(table, mask, fnv1a64(s + sl + 1, len - sl - 1))) { out[i] = 1; return; }
    }
    out[i] = flags;
}

// Tier-2: full find_best_match scoring (name_matcher/support.rs), exact in
// scaled x10 integers (order-preserving vs the CPU's f64: every CPU term is a
// multiple of 0.1 except the line-distance term, which scales exactly).
// One thread per reference scans its CSR candidate slice IN ORDER (CPU
// tie-break is strict-> first-wins, so identical order = identical pick):
//   same file            +1000
//   dir-prefix proximity +150/shared segment, cap 800
//   same language        +500 else -800
//   Calls -> Fn|Method   +250
//   Instantiates -> Class|Struct|Interface +250
//   Decorates -> Fn|Method +250, Class|Interface +150
//   exported             +100
//   same-file line dist  max(0, 200 - |dline|)   [cand line != 0]
extern "C" __global__ void score_candidates(
    const int*  __restrict__ ref_group,    // n_refs: CSR group id or -1
    const u32*  __restrict__ ref_file,     // n_refs: interned file id
    const u8*   __restrict__ ref_lang,     // n_refs
    const u8*   __restrict__ ref_kind,     // n_refs: 1=Calls 2=Instantiates 3=Decorates 0=other
    const u32*  __restrict__ ref_line,     // n_refs
    const u32*  __restrict__ cand_starts,  // n_groups + 1 (CSR)
    const u32*  __restrict__ cand_file,
    const u8*   __restrict__ cand_lang,
    const u8*   __restrict__ cand_kind,    // 1=Fn 2=Method 3=Class 4=Struct 5=Interface 0=other
    const u8*   __restrict__ cand_exported,
    const u32*  __restrict__ cand_line,
    const u32*  __restrict__ dir_starts,   // n_files + 1: per-file dir-hash CSR
    const u64*  __restrict__ dir_hashes,
    int*        __restrict__ out_best,     // n_refs: best candidate idx or -1
    u32 n_refs
) {
    u32 i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n_refs) return;
    int g = ref_group[i];
    out_best[i] = -1;
    if (g < 0) return;
    u32 s = cand_starts[g], e = cand_starts[g + 1];
    u32 rf = ref_file[i];
    u32 rds = dir_starts[rf], rde = dir_starts[rf + 1];
    // CPU best_score starts at -1.0 with strict > : a candidate scoring
    // below that (cross-language -800 dominating) is NEVER selected. x10
    // scale -> initialize to -10 for exact parity.
    long long best_score = -10;
    int best = -1;
    for (u32 c = s; c < e; c++) {
        long long score = 0;
        u32 cf = cand_file[c];
        if (cf == rf) score += 1000;
        // proximity: shared leading dir-hash segments
        u32 cds = dir_starts[cf], cde = dir_starts[cf + 1];
        u32 n1 = rde - rds, n2 = cde - cds;
        u32 lim = n1 < n2 ? n1 : n2;
        long long shared = 0;
        for (u32 k = 0; k < lim; k++) {
            if (dir_hashes[rds + k] == dir_hashes[cds + k]) shared++;
            else break;
        }
        long long prox = shared * 150; if (prox > 800) prox = 800;
        score += prox;
        score += (cand_lang[c] == ref_lang[i]) ? 500 : -800;
        u8 rk = ref_kind[i], ck = cand_kind[c];
        if (rk == 1 && (ck == 1 || ck == 2)) score += 250;
        if (rk == 2 && (ck == 3 || ck == 4 || ck == 5)) score += 250;
        if (rk == 3) {
            if (ck == 1 || ck == 2) score += 250;
            else if (ck == 3 || ck == 5) score += 150;
        }
        if (cand_exported[c]) score += 100;
        if (cf == rf && cand_line[c] != 0) {
            long long d = (long long)cand_line[c] - (long long)ref_line[i];
            if (d < 0) d = -d;
            long long lt = 200 - d; if (lt < 0) lt = 0;
            score += lt;
        }
        if (score > best_score) { best_score = score; best = (int)c; }
    }
    out_best[i] = best;
}

// Tier-3: match_method_call strategies 1+2 (name_matcher/method.rs).
// For each reference: walk its class-candidate list IN ORDER; for each class,
// scan the methods of that class's file IN ORDER; first method whose
// name-hash matches the ref's method name AND whose qualified_name CONTAINS
// the class name wins (the CPU uses Iterator::find on both levels, so
// order-preserving first-match = identical selection).
extern "C" __device__ bool contains_bytes(
    const u8* hay, u32 hay_len, const u8* needle, u32 needle_len
) {
    if (needle_len == 0) return true;
    if (needle_len > hay_len) return false;
    for (u32 i = 0; i + needle_len <= hay_len; i++) {
        u32 j = 0;
        while (j < needle_len && hay[i + j] == needle[j]) j++;
        if (j == needle_len) return true;
    }
    return false;
}

extern "C" __global__ void match_class_methods(
    const u32* __restrict__ ref_cand_starts,   // n_refs+1: CSR into class-candidate arrays
    const u64* __restrict__ ref_method_hash,   // n_refs: FNV of the method name
    const u32* __restrict__ cls_file,          // per class-candidate: file id
    const u32* __restrict__ cls_name_off,      // per class-candidate: offset into name buf
    const u32* __restrict__ cls_name_len,
    const u8*  __restrict__ name_buf,
    const u32* __restrict__ file_starts,       // n_files+1: CSR into method arrays
    const u64* __restrict__ m_hash,            // per method: FNV of method name
    const u32* __restrict__ m_qn_off,          // per method: offset into qn buf
    const u32* __restrict__ m_qn_len,
    const u8*  __restrict__ qn_buf,
    int*       __restrict__ out_method,        // n_refs: winning method idx or -1
    int*       __restrict__ out_cls,           // n_refs: winning class-candidate idx or -1
    u32 n_refs
) {
    u32 i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n_refs) return;
    out_method[i] = -1; out_cls[i] = -1;
    u64 want = ref_method_hash[i];
    for (u32 c = ref_cand_starts[i]; c < ref_cand_starts[i + 1]; c++) {
        u32 f = cls_file[c];
        const u8* cname = name_buf + cls_name_off[c];
        u32 clen = cls_name_len[c];
        for (u32 m = file_starts[f]; m < file_starts[f + 1]; m++) {
            if (m_hash[m] != want) continue;
            if (contains_bytes(qn_buf + m_qn_off[m], m_qn_len[m], cname, clen)) {
                out_method[i] = (int)m;
                out_cls[i] = (int)c;
                return; // first match wins, both levels in order
            }
        }
    }
}

// Tier-4: match_fuzzy (name_matcher/fuzzy.rs) — lowercase-name candidates,
// callable kinds only (Function=1, Method=2, Class=3), prefer same-language;
// a winner exists ONLY when the preferred set has exactly one member.
// out: candidate idx or -1; out_cross: 1 when the winner is cross-language.
extern "C" __global__ void fuzzy_unique(
    const int* __restrict__ ref_group,   // n_refs: CSR group or -1
    const u8*  __restrict__ ref_lang,
    const u32* __restrict__ cand_starts, // n_groups + 1
    const u8*  __restrict__ cand_lang,
    const u8*  __restrict__ cand_kind,   // kind classes as in score_candidates
    int*       __restrict__ out_idx,
    u8*        __restrict__ out_cross,
    u32 n_refs
) {
    u32 i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n_refs) return;
    out_idx[i] = -1; out_cross[i] = 0;
    int g = ref_group[i];
    if (g < 0) return;
    u32 s = cand_starts[g], e = cand_starts[g + 1];
    u8 rl = ref_lang[i];
    int same = -1, any = -1;
    u32 same_n = 0, any_n = 0;
    for (u32 c = s; c < e; c++) {
        u8 k = cand_kind[c];
        if (!(k == 1 || k == 2 || k == 3)) continue; // callable only
        any_n++; any = (int)c;
        if (cand_lang[c] == rl) { same_n++; same = (int)c; }
    }
    if (same_n == 1) { out_idx[i] = same; out_cross[i] = 0; }
    else if (same_n == 0 && any_n == 1) { out_idx[i] = any; out_cross[i] = 1; }
}
