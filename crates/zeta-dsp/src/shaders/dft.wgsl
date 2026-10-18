// Direct DFT power: one invocation per output bin k, |sum_n x[n] * tw[(k*n) mod N]|^2.
// tw[m] = exp(-j*2*pi*m/N), precomputed on the host in f64 so the result does not
// depend on the device's sin/cos precision. O(N^2): a proof of the CPU/GPU stage
// boundary, to be replaced by a radix-2 FFT when throughput matters.

struct Params {
    n: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> x: array<vec2<f32>>;
@group(0) @binding(1) var<storage, read> tw: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
@group(0) @binding(3) var<uniform> params: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = params.n;
    let k = gid.x;
    if (k >= n) {
        return;
    }
    var re = 0.0;
    var im = 0.0;
    var m = 0u;
    for (var i = 0u; i < n; i = i + 1u) {
        let s = x[i];
        let w = tw[m];
        re = re + s.x * w.x - s.y * w.y;
        im = im + s.x * w.y + s.y * w.x;
        // m = (k * i) mod n without overflow; k < n so one subtraction suffices.
        m = m + k;
        if (m >= n) {
            m = m - n;
        }
    }
    out[k] = re * re + im * im;
}
