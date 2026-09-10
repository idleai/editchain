/**
 * The active graph renderer: the per-row SVG cells (`svg`).
 * @returns {string}
 */
export function debugBackend() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.debugBackend();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * `__editchainDataReady` mirror: content has rendered for the view.
 * @returns {boolean}
 */
export function debugDataReady() {
    const ret = wasm.debugDataReady();
    return ret !== 0;
}

/**
 * `__editchainRendererDebug.findState()` data source: the settled find session
 * (read-only parity facade; never app state).
 * @returns {string}
 */
export function debugFindState() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.debugFindState();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * DOM generation counter (render passes) for `whenIdle` stability checks.
 * @returns {bigint}
 */
export function debugGeneration() {
    const ret = wasm.debugGeneration();
    return BigInt.asUintN(64, ret);
}

/**
 * `__editchainGraphState` data source (render window + lane geometry).
 * @returns {string}
 */
export function debugGraphState() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.debugGraphState();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * `__editchainInFlightCount` data source.
 * @returns {bigint}
 */
export function debugInFlightCount() {
    const ret = wasm.debugInFlightCount();
    return BigInt.asUintN(64, ret);
}

/**
 * `__editchainGraphAdapter.laneXAll` data source (fixed CSS-px centers).
 * @returns {string}
 */
export function debugLaneXAll() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.debugLaneXAll();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * `__editchainRendererDebug.metrics()` data source (bootstrap field names).
 * @returns {string}
 */
export function debugMetrics() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.debugMetrics();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * Successful per-row SVG render passes.
 * @returns {bigint}
 */
export function debugRenderCount() {
    const ret = wasm.debugRenderCount();
    return BigInt.asUintN(64, ret);
}

/**
 * `__editchainRendererInstanceId` data source.
 * @returns {string}
 */
export function debugRendererInstanceId() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.debugRendererInstanceId();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * `__editchainRowAt` data source: the cached row JSON at an absolute index.
 * @param {bigint} abs
 * @returns {string}
 */
export function debugRowAt(abs) {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.debugRowAt(abs);
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * `__editchainRendererDebug.snapshot()` data source.
 * @returns {string}
 */
export function debugSnapshot() {
    let deferred1_0;
    let deferred1_1;
    try {
        const ret = wasm.debugSnapshot();
        deferred1_0 = ret[0];
        deferred1_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
    }
}

/**
 * `__editchainGetTotal` data source: the authoritative history total, or
 * `-1` while unknown.
 * @returns {number}
 */
export function debugTotal() {
    const ret = wasm.debugTotal();
    return ret;
}

/**
 * `__editchainViewGen` data source.
 * @returns {bigint}
 */
export function debugViewGen() {
    const ret = wasm.debugViewGen();
    return BigInt.asUintN(64, ret);
}

/**
 * WASM startup entry: install the Rust shell (listener before
 * `webviewReady`) and mark the per-row SVG renderer ready. No canvas
 * surface is created on this path — the graph lives inside the scrolling
 * row DOM, so there is nothing to position or chase.
 *
 * # Errors
 *
 * Returns an error when the shell scaffold or the VS Code API is
 * unavailable.
 */
export function startHistoryView() {
    const ret = wasm.startHistoryView();
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_debug_string_a57024b9c6e4a48b: function(arg0, arg1) {
            const ret = debugString(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_5e4570eb24ffa122: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_undefined_6cff064c44e0d823: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_number_get_136b9679cab35cfb: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_string_get_d154f1e671052120: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_bb96b2010945f0bc: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_be22cc64ae6946a0: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_acquireVsCodeApi_2007e6d5dba5eb83: function() { return handleError(function () {
            const ret = acquireVsCodeApi();
            return ret;
        }, arguments); },
        __wbg_activeElement_d26d0a6ad4770cf7: function(arg0) {
            const ret = arg0.activeElement;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_addEventListener_3b8edc02c33d9f77: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.addEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_add_43c69c0af85151c4: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.add(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_appendChild_d5cbce3d5fa81471: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.appendChild(arg1);
            return ret;
        }, arguments); },
        __wbg_apply_6cd9daeacef161ce: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.apply(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_body_d6eca0586d628e3c: function(arg0) {
            const ret = arg0.body;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_bottom_364a8da495eed98e: function(arg0) {
            const ret = arg0.bottom;
            return ret;
        },
        __wbg_classList_23983f0a4979ea93: function(arg0) {
            const ret = arg0.classList;
            return ret;
        },
        __wbg_clearInterval_db08de379bb142dc: function(arg0, arg1) {
            arg0.clearInterval(arg1);
        },
        __wbg_clearTimeout_4e61cface6c91ad9: function(arg0, arg1) {
            arg0.clearTimeout(arg1);
        },
        __wbg_clientHeight_834c029be3d903a7: function(arg0) {
            const ret = arg0.clientHeight;
            return ret;
        },
        __wbg_clientWidth_ad03e8eb6c2b0c56: function(arg0) {
            const ret = arg0.clientWidth;
            return ret;
        },
        __wbg_clientX_73f0b294f91b259a: function(arg0) {
            const ret = arg0.clientX;
            return ret;
        },
        __wbg_closest_7eff62c8f6e11be3: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.closest(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_createDocumentFragment_64c2d174cf854823: function(arg0) {
            const ret = arg0.createDocumentFragment();
            return ret;
        },
        __wbg_createElementNS_f18ede2d74f15ea1: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            const ret = arg0.createElementNS(arg1 === 0 ? undefined : getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
            return ret;
        }, arguments); },
        __wbg_createElement_7f42344eee7bb810: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.createElement(getStringFromWasm0(arg1, arg2));
            return ret;
        }, arguments); },
        __wbg_createTextNode_f5ee2b1cd3e249bb: function(arg0, arg1, arg2) {
            const ret = arg0.createTextNode(getStringFromWasm0(arg1, arg2));
            return ret;
        },
        __wbg_data_57d8ce4eb5f0a433: function(arg0) {
            const ret = arg0.data;
            return ret;
        },
        __wbg_detail_792afe1f5dd13f96: function(arg0) {
            const ret = arg0.detail;
            return ret;
        },
        __wbg_disconnect_0f608ec00c13c91e: function(arg0) {
            arg0.disconnect();
        },
        __wbg_document_ac38448dbfd31a57: function(arg0) {
            const ret = arg0.document;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_error_dd408a7b3cb542dd: function(arg0) {
            console.error(arg0);
        },
        __wbg_firstChild_078b3f58c509c470: function(arg0) {
            const ret = arg0.firstChild;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_focus_77d7483c7b2b9f30: function() { return handleError(function (arg0) {
            arg0.focus();
        }, arguments); },
        __wbg_focus_c7d4fe3aba923a18: function() { return handleError(function (arg0, arg1) {
            arg0.focus(arg1);
        }, arguments); },
        __wbg_getAttribute_4c6e1df05f9ee034: function(arg0, arg1, arg2, arg3) {
            const ret = arg1.getAttribute(getStringFromWasm0(arg2, arg3));
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_getBoundingClientRect_8165472d0fc91753: function(arg0) {
            const ret = arg0.getBoundingClientRect();
            return ret;
        },
        __wbg_getElementById_1637d6969b003cda: function(arg0, arg1, arg2) {
            const ret = arg0.getElementById(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_get_971a0c45d172643f: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_height_949ea86de6d5d939: function(arg0) {
            const ret = arg0.height;
            return ret;
        },
        __wbg_info_726982aff9befe16: function(arg0) {
            console.info(arg0);
        },
        __wbg_innerWidth_4a973110eeb5f463: function() { return handleError(function (arg0) {
            const ret = arg0.innerWidth;
            return ret;
        }, arguments); },
        __wbg_insertBefore_f3733e91a030079e: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.insertBefore(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_instanceof_Element_a3960bb00f4964bc: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Element;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlButtonElement_806e934e95055a80: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLButtonElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlDivElement_dea71919891d3c61: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLDivElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlElement_6b02a3740edba922: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlInputElement_6077656bcaf1eb33: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLInputElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Node_ad9597995317f467: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Node;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_5625ff9937037a38: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_item_e5f9a04853abaf70: function(arg0, arg1) {
            const ret = arg0.item(arg1 >>> 0);
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_key_d1b2fd5ee42567c0: function(arg0, arg1) {
            const ret = arg1.key;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_length_19ed3d1851f7cc4f: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_length_7afd83ae4ddf324f: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_new_116be93542d39019: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_new_4c27d8e40bf066b2: function() { return handleError(function (arg0) {
            const ret = new ResizeObserver(arg0);
            return ret;
        }, arguments); },
        __wbg_new_ebe3e0f6837f0879: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_now_2283802bfbda617e: function(arg0) {
            const ret = arg0.now();
            return ret;
        },
        __wbg_now_8b265300afd5f2b9: function() {
            const ret = Date.now();
            return ret;
        },
        __wbg_observe_77ac29d55bfbcc1c: function(arg0, arg1) {
            arg0.observe(arg1);
        },
        __wbg_offsetLeft_67e583681656bca0: function(arg0) {
            const ret = arg0.offsetLeft;
            return ret;
        },
        __wbg_offsetWidth_5d9c7950b81e03f2: function(arg0) {
            const ret = arg0.offsetWidth;
            return ret;
        },
        __wbg_parentNode_8634e029370ec1bb: function(arg0) {
            const ret = arg0.parentNode;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_parse_1cc93481b0865939: function() { return handleError(function (arg0, arg1) {
            const ret = JSON.parse(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_performance_821a3767f0dce300: function(arg0) {
            const ret = arg0.performance;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_preventDefault_19878c58b8010668: function(arg0) {
            arg0.preventDefault();
        },
        __wbg_previousElementSibling_b512200781bbfd5f: function(arg0) {
            const ret = arg0.previousElementSibling;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_push_adb0107829f02d75: function(arg0, arg1) {
            const ret = arg0.push(arg1);
            return ret;
        },
        __wbg_querySelectorAll_02dbbd6bb849b369: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.querySelectorAll(getStringFromWasm0(arg1, arg2));
            return ret;
        }, arguments); },
        __wbg_querySelector_839d6534e69c0f64: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.querySelector(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_queueMicrotask_ac694eae12e92dfb: function(arg0) {
            queueMicrotask(arg0);
        },
        __wbg_queueMicrotask_be5fe34a8f4cad4d: function(arg0) {
            const ret = arg0.queueMicrotask;
            return ret;
        },
        __wbg_random_b0d98802be10ff20: function() {
            const ret = Math.random();
            return ret;
        },
        __wbg_removeAttribute_bb10532a6f012605: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.removeAttribute(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_removeEventListener_aa653c6b402cc27e: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.removeEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_remove_07453fe173d20eee: function(arg0) {
            arg0.remove();
        },
        __wbg_remove_426d5806a1a02ede: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.remove(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_replaceChild_ed091b5afced149b: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.replaceChild(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_resolve_020f95d838c6ef25: function(arg0) {
            const ret = Promise.resolve(arg0);
            return ret;
        },
        __wbg_scrollTop_dacf35eddc723d68: function(arg0) {
            const ret = arg0.scrollTop;
            return ret;
        },
        __wbg_scrollWidth_b353aef788aa0bc6: function(arg0) {
            const ret = arg0.scrollWidth;
            return ret;
        },
        __wbg_setAttribute_507f8367905a9c03: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.setAttribute(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_setInterval_4da81e87aef31371: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.setInterval(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_setProperty_684ce273e28a7037: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.setProperty(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_setTimeout_8be4960d8ad2bb76: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.setTimeout(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_set_8155bb79a948541b: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_set_className_bc6ed54ffff19a12: function(arg0, arg1, arg2) {
            arg0.className = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_disabled_a44ebf7a81556935: function(arg0, arg1) {
            arg0.disabled = arg1 !== 0;
        },
        __wbg_set_hidden_bad5dd4b41558b96: function(arg0, arg1) {
            arg0.hidden = arg1 !== 0;
        },
        __wbg_set_prevent_scroll_012725f8a1602bdd: function(arg0, arg1) {
            arg0.preventScroll = arg1 !== 0;
        },
        __wbg_set_scrollTop_95b38daef1936437: function(arg0, arg1) {
            arg0.scrollTop = arg1;
        },
        __wbg_set_textContent_e027901c7bc836b5: function(arg0, arg1, arg2) {
            arg0.textContent = arg1 === 0 ? undefined : getStringFromWasm0(arg1, arg2);
        },
        __wbg_shiftKey_8eca009f693152b4: function(arg0) {
            const ret = arg0.shiftKey;
            return ret;
        },
        __wbg_slice_cc0f63edb5397a2d: function(arg0, arg1, arg2) {
            const ret = arg0.slice(arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_static_accessor_GLOBAL_THIS_466428f93b4eaa76: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_c7aea38d4de089bc: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_SELF_42d4fae05e59267a: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_e0db14a0eba6a812: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_stopPropagation_13b9aec9ab20a836: function(arg0) {
            arg0.stopPropagation();
        },
        __wbg_stringify_f93a4ebae9231922: function() { return handleError(function (arg0) {
            const ret = JSON.stringify(arg0);
            return ret;
        }, arguments); },
        __wbg_style_f09d6445af3dd2c6: function(arg0) {
            const ret = arg0.style;
            return ret;
        },
        __wbg_target_13424fe1cdc436ac: function(arg0) {
            const ret = arg0.target;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_then_7026b513a94278a8: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_then_72819b8d4e081fb5: function(arg0, arg1, arg2) {
            const ret = arg0.then(arg1, arg2);
            return ret;
        },
        __wbg_toString_42552a6ec8683b15: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.toString(arg1);
            return ret;
        }, arguments); },
        __wbg_top_66d56bb4eca7d9c5: function(arg0) {
            const ret = arg0.top;
            return ret;
        },
        __wbg_value_35f0fb42e7c3d468: function(arg0, arg1) {
            const ret = arg1.value;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 165, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___wasm_bindgen_a267d00861f6ecb8___JsValue__core_7d5f0a2ba6a62c33___result__Result_____wasm_bindgen_a267d00861f6ecb8___JsError___true_);
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [F64], shim_idx: 120, ret: Externref, inner_ret: Some(Externref) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___f64__wasm_bindgen_a267d00861f6ecb8___JsValue__true_);
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("Event")], shim_idx: 117, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___web_sys_8491e82b117b16ac___features__gen_KeyboardEvent__KeyboardEvent______true_);
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("KeyboardEvent")], shim_idx: 117, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___web_sys_8491e82b117b16ac___features__gen_KeyboardEvent__KeyboardEvent______true__3);
            return ret;
        },
        __wbindgen_cast_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Vector(NamedExternref("ResizeObserverEntry")), NamedExternref("ResizeObserver")], shim_idx: 115, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___alloc_46d95d5090e9754d___vec__Vec_web_sys_8491e82b117b16ac___features__gen_ResizeObserverEntry__ResizeObserverEntry___web_sys_8491e82b117b16ac___features__gen_ResizeObserver__ResizeObserver______true_);
            return ret;
        },
        __wbindgen_cast_0000000000000006: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 122, ret: F64, inner_ret: Some(F64) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___f64__true_);
            return ret;
        },
        __wbindgen_cast_0000000000000007: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 124, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke_______true_);
            return ret;
        },
        __wbindgen_cast_0000000000000008: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000009: function(arg0, arg1) {
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
        "./editchain_history_renderer_bg.js": import0,
    };
}

function wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke_______true_(arg0, arg1) {
    wasm.wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke_______true_(arg0, arg1);
}

function wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___f64__true_(arg0, arg1) {
    const ret = wasm.wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___f64__true_(arg0, arg1);
    return ret;
}

function wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___web_sys_8491e82b117b16ac___features__gen_KeyboardEvent__KeyboardEvent______true_(arg0, arg1, arg2) {
    wasm.wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___web_sys_8491e82b117b16ac___features__gen_KeyboardEvent__KeyboardEvent______true_(arg0, arg1, arg2);
}

function wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___web_sys_8491e82b117b16ac___features__gen_KeyboardEvent__KeyboardEvent______true__3(arg0, arg1, arg2) {
    wasm.wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___web_sys_8491e82b117b16ac___features__gen_KeyboardEvent__KeyboardEvent______true__3(arg0, arg1, arg2);
}

function wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___wasm_bindgen_a267d00861f6ecb8___JsValue__core_7d5f0a2ba6a62c33___result__Result_____wasm_bindgen_a267d00861f6ecb8___JsError___true_(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___wasm_bindgen_a267d00861f6ecb8___JsValue__core_7d5f0a2ba6a62c33___result__Result_____wasm_bindgen_a267d00861f6ecb8___JsError___true_(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___alloc_46d95d5090e9754d___vec__Vec_web_sys_8491e82b117b16ac___features__gen_ResizeObserverEntry__ResizeObserverEntry___web_sys_8491e82b117b16ac___features__gen_ResizeObserver__ResizeObserver______true_(arg0, arg1, arg2, arg3) {
    const ptr0 = passArrayJsValueToWasm0(arg2, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    wasm.wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___alloc_46d95d5090e9754d___vec__Vec_web_sys_8491e82b117b16ac___features__gen_ResizeObserverEntry__ResizeObserverEntry___web_sys_8491e82b117b16ac___features__gen_ResizeObserver__ResizeObserver______true_(arg0, arg1, ptr0, len0, arg3);
}

function wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___f64__wasm_bindgen_a267d00861f6ecb8___JsValue__true_(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_a267d00861f6ecb8___convert__closures_____invoke___f64__wasm_bindgen_a267d00861f6ecb8___JsValue__true_(arg0, arg1, arg2);
    return ret;
}

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => wasm.__wbindgen_destroy_closure(state.a, state.b));

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

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

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, f) {
    const state = { a: arg0, b: arg1, cnt: 1 };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            wasm.__wbindgen_destroy_closure(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passArrayJsValueToWasm0(array, malloc) {
    const ptr = malloc(array.length * 4, 4) >>> 0;
    for (let i = 0; i < array.length; i++) {
        const add = addToExternrefTable0(array[i]);
        getDataViewMemory0().setUint32(ptr + 4 * i, add, true);
    }
    WASM_VECTOR_LEN = array.length;
    return ptr;
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
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
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

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (!module.ok) {
            throw new Error(`failed to fetch Wasm: ${module.status} ${module.statusText} fetching '${module.url}'`);
        }

        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('editchain_history_renderer_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
