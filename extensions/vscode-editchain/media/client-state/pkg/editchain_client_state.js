/**
 * Shared reconnect policy, usable by the Node extension and browser hosts.
 */
class SharedConnection {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        SharedConnectionFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_sharedconnection_free(ptr, 0);
    }
    /**
     * A transport is open but its peer has not been authenticated.
     * @param {number} generation
     */
    authenticating(generation) {
        wasm.sharedconnection_authenticating(this.__wbg_ptr, generation);
    }
    /**
     * Start an attempt and return its callback token.
     *
     * # Errors
     * Fails when callback identities cannot be allocated without reuse.
     * @returns {number}
     */
    begin() {
        const ret = wasm.sharedconnection_begin(this.__wbg_ptr);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * A new invitation is required before retrying.
     * @param {number} generation
     */
    expired(generation) {
        wasm.sharedconnection_expired(this.__wbg_ptr, generation);
    }
    /**
     * Current callback token.
     * @returns {number}
     */
    get generation() {
        const ret = wasm.sharedconnection_generation(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Create a stopped connection.
     */
    constructor() {
        const ret = wasm.sharedconnection_new();
        this.__wbg_ptr = ret;
        SharedConnectionFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Project validated native inventory progress into shared status.
     *
     * # Errors
     * Rejects malformed progress without changing the current connection state.
     * @param {number} generation
     * @param {string} json
     */
    progress(generation, json) {
        const ptr0 = passStringToWasm0(json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.sharedconnection_progress(this.__wbg_ptr, generation, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Hosting is ready, or a peer inventory has been reconciled.
     * @param {number} generation
     */
    ready(generation) {
        wasm.sharedconnection_ready(this.__wbg_ptr, generation);
    }
    /**
     * Bounded retry delay; the host supplies its timer and optional jitter.
     * @returns {number}
     */
    get retry_delay_ms() {
        const ret = wasm.sharedconnection_retry_delay_ms(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Shared presentable status.
     * @returns {string}
     */
    get status() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.sharedconnection_status(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Retire callbacks from a stopped transport.
     */
    stop() {
        wasm.sharedconnection_stop(this.__wbg_ptr);
    }
    /**
     * Record a retryable failure for its owning attempt.
     * @param {number} generation
     */
    waiting(generation) {
        wasm.sharedconnection_waiting(this.__wbg_ptr, generation);
    }
}
if (Symbol.dispose) SharedConnection.prototype[Symbol.dispose] = SharedConnection.prototype.free;
exports.SharedConnection = SharedConnection;

/**
 * Join lifetime shared with app-core; the host retains credentials and transports.
 */
class SharedJoin {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        SharedJoinFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_sharedjoin_free(ptr, 0);
    }
    /**
     * Accept a completed join only in its original context.
     * @param {number} generation
     * @returns {boolean}
     */
    enable(generation) {
        const ret = wasm.sharedjoin_enable(this.__wbg_ptr, generation);
        return ret !== 0;
    }
    /**
     * Whether sharing has been enabled by a current approval.
     * @returns {boolean}
     */
    get enabled() {
        const ret = wasm.sharedjoin_enabled(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * Current token for asynchronous host work.
     * @returns {number}
     */
    get generation() {
        const ret = wasm.sharedjoin_generation(this.__wbg_ptr);
        return ret >>> 0;
    }
    /**
     * Check that pending host work still owns the join lifetime.
     * @param {number} generation
     * @returns {boolean}
     */
    is_current(generation) {
        const ret = wasm.sharedjoin_is_current(this.__wbg_ptr, generation);
        return ret !== 0;
    }
    /**
     * Create a stopped sharing session.
     */
    constructor() {
        const ret = wasm.sharedjoin_new();
        this.__wbg_ptr = ret;
        SharedJoinFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Stop/suspend and retire all previous host callbacks.
     */
    retire() {
        wasm.sharedjoin_retire(this.__wbg_ptr);
    }
}
if (Symbol.dispose) SharedJoin.prototype[Symbol.dispose] = SharedJoin.prototype.free;
exports.SharedJoin = SharedJoin;
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_throw_bb96b2010945f0bc: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./editchain_client_state_bg.js": import0,
    };
}

const SharedConnectionFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_sharedconnection_free(ptr, 1));
const SharedJoinFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_sharedjoin_free(ptr, 1));

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
function decodeText(ptr, len) {
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

const wasmPath = `${__dirname}/editchain_client_state_bg.wasm`;
const wasmBytes = require('fs').readFileSync(wasmPath);
const wasmModule = new WebAssembly.Module(wasmBytes);
let wasmInstance = new WebAssembly.Instance(wasmModule, __wbg_get_imports());
let wasm = wasmInstance.exports;
wasm.__wbindgen_start();
