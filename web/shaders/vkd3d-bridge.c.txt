/* SPDX-License-Identifier: MIT
 * Browser-facing ABI for unmodified LGPL vkd3d-shader 2.1. DXBC carries its
 * shader stage; no application-specific shader substitutions are made here.
 */
#include "vkd3d_shader.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define WB_MAX_DXBC (1024u * 1024u)
#define WB_MAX_SPIRV (16u * 1024u * 1024u)
#define WB_MAX_MESSAGES 8192u
#define WB_LEGACY_STAGES 2u
#define WB_MAX_LEGACY_BINDINGS 64u
#define WB_MAX_LEGACY_VARYINGS 12u

/*
 * Descriptor scanning and canonical D3D12 binding support. These tables are
 * declared here so wb_clear() can reset them; the entry points that fill them
 * are defined further down, next to the root-signature inspection code.
 */
#define WB_MAX_SCAN_DESCRIPTORS 4096u
#define WB_DESCRIPTOR_RECORD 7u
#define WB_MAX_ROOT_PARAMETERS 64u
#define WB_MAX_ROOT_RANGES 128u
#define WB_MAX_ROOT_SAMPLERS 64u
#define WB_ROOT_HEADER_WORDS 6u
#define WB_ROOT_PARAMETER_WORDS 7u
#define WB_ROOT_RANGE_WORDS 5u
#define WB_ROOT_SAMPLER_WORDS 11u

static uint32_t wb_descriptor_results[WB_MAX_SCAN_DESCRIPTORS * WB_DESCRIPTOR_RECORD];
static unsigned int wb_descriptor_count;
static uint32_t wb_root_words[WB_ROOT_HEADER_WORDS
        + WB_MAX_ROOT_PARAMETERS * WB_ROOT_PARAMETER_WORDS
        + WB_MAX_ROOT_RANGES * WB_ROOT_RANGE_WORDS
        + WB_MAX_ROOT_SAMPLERS * WB_ROOT_SAMPLER_WORDS];
static unsigned int wb_root_word_count;

static struct vkd3d_shader_code wb_result;
static struct vkd3d_shader_code wb_legacy_results[WB_LEGACY_STAGES];
static struct vkd3d_shader_scan_signature_info wb_legacy_signatures[WB_LEGACY_STAGES];
static struct vkd3d_shader_scan_descriptor_info wb_legacy_descriptors[WB_LEGACY_STAGES];
static char wb_messages[WB_MAX_MESSAGES];
static unsigned int wb_root_flags;

static void wb_capture_messages(char *messages)
{
    if (messages)
    {
        snprintf(wb_messages, sizeof(wb_messages), "%s", messages);
        vkd3d_shader_free_messages(messages);
    }
}

void wb_clear(void)
{
    unsigned int i;

    vkd3d_shader_free_shader_code(&wb_result);
    memset(&wb_result, 0, sizeof(wb_result));
    for (i = 0; i < WB_LEGACY_STAGES; ++i)
    {
        vkd3d_shader_free_shader_code(&wb_legacy_results[i]);
        vkd3d_shader_free_scan_signature_info(&wb_legacy_signatures[i]);
        vkd3d_shader_free_scan_descriptor_info(&wb_legacy_descriptors[i]);
        memset(&wb_legacy_results[i], 0, sizeof(wb_legacy_results[i]));
        memset(&wb_legacy_signatures[i], 0, sizeof(wb_legacy_signatures[i]));
        memset(&wb_legacy_descriptors[i], 0, sizeof(wb_legacy_descriptors[i]));
    }
    wb_messages[0] = '\0';
    wb_root_flags = 0;
    wb_descriptor_count = 0;
    wb_root_word_count = 0;
}

static int wb_legacy_validate(unsigned int stage, const void *bytes, unsigned int length)
{
    uint32_t version, end;
    unsigned int major, minor, expected;

    if (stage >= WB_LEGACY_STAGES || !bytes || length < 8 || length > WB_MAX_DXBC || length % 4)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Legacy shader input must be 8..%u bytes and DWORD-aligned", WB_MAX_DXBC);
        return 0;
    }
    memcpy(&version, bytes, sizeof(version));
    memcpy(&end, (const uint8_t *)bytes + length - sizeof(end), sizeof(end));
    expected = stage ? 0xffffu : 0xfffeu;
    major = (version >> 8) & 0xffu;
    minor = version & 0xffu;
    if ((version >> 16) != expected
            || (stage == 0 && !((major == 1 && minor == 1)
                    || ((major == 2 || major == 3) && minor == 0)))
            || (stage == 1 && !((major == 1 && minor <= 4)
                    || ((major == 2 || major == 3) && minor == 0))))
    {
        snprintf(wb_messages, sizeof(wb_messages), "Legacy shader stage/version %#x is unsupported", version);
        return 0;
    }
    if (end != 0x0000ffffu)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Legacy shader END token is missing");
        return 0;
    }
    return 1;
}

static int wb_legacy_scan(unsigned int stage, const void *bytes, unsigned int length)
{
    struct vkd3d_shader_compile_info info = {0};
    char *messages = NULL;
    int result;

    wb_legacy_signatures[stage].type = VKD3D_SHADER_STRUCTURE_TYPE_SCAN_SIGNATURE_INFO;
    wb_legacy_descriptors[stage].type = VKD3D_SHADER_STRUCTURE_TYPE_SCAN_DESCRIPTOR_INFO;
    wb_legacy_descriptors[stage].next = &wb_legacy_signatures[stage];
    info.type = VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO;
    info.next = &wb_legacy_descriptors[stage];
    info.source.code = bytes;
    info.source.size = length;
    info.source_type = VKD3D_SHADER_SOURCE_D3D_BYTECODE;
    info.target_type = VKD3D_SHADER_TARGET_SPIRV_BINARY;
    info.log_level = VKD3D_SHADER_LOG_WARNING;
    result = vkd3d_shader_scan(&info, &messages);
    wb_capture_messages(messages);
    if (result < 0)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "Legacy shader scan failed (%d)", result);
        return 0;
    }
    return 1;
}

static int wb_legacy_compile_stage(unsigned int stage, const void *bytes, unsigned int length,
        const struct vkd3d_shader_varying_map_info *varying_map)
{
    const struct vkd3d_shader_compile_option options[] =
    {
        {VKD3D_SHADER_COMPILE_OPTION_WRITE_TESS_GEOM_POINT_SIZE, 0},
    };
    struct vkd3d_shader_resource_binding bindings[WB_MAX_LEGACY_BINDINGS];
    struct vkd3d_shader_d3dbc_source_info source = {0};
    struct vkd3d_shader_spirv_target_info target = {0};
    struct vkd3d_shader_interface_info interface = {0};
    struct vkd3d_shader_compile_info info = {0};
    const struct vkd3d_shader_descriptor_info *descriptor;
    struct vkd3d_shader_resource_binding *binding;
    unsigned int i, group = stage;
    char *messages = NULL;
    int result;

    if (wb_legacy_descriptors[stage].descriptor_count > WB_MAX_LEGACY_BINDINGS)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Legacy shader descriptor count exceeds %u", WB_MAX_LEGACY_BINDINGS);
        return 0;
    }
    for (i = 0; i < wb_legacy_descriptors[stage].descriptor_count; ++i)
    {
        descriptor = &wb_legacy_descriptors[stage].descriptors[i];
        binding = &bindings[i];
        memset(binding, 0, sizeof(*binding));
        if (descriptor->register_space || descriptor->count != 1
                || (descriptor->type != VKD3D_SHADER_DESCRIPTOR_TYPE_CBV
                    && descriptor->type != VKD3D_SHADER_DESCRIPTOR_TYPE_SRV
                    && descriptor->type != VKD3D_SHADER_DESCRIPTOR_TYPE_SAMPLER))
        {
            snprintf(wb_messages, sizeof(wb_messages), "Unsupported legacy descriptor type or range");
            return 0;
        }
        binding->type = descriptor->type;
        binding->register_index = descriptor->register_index;
        binding->shader_visibility = stage ? VKD3D_SHADER_VISIBILITY_PIXEL : VKD3D_SHADER_VISIBILITY_VERTEX;
        binding->binding.set = group;
        binding->binding.count = 1;
        if (descriptor->type == VKD3D_SHADER_DESCRIPTOR_TYPE_CBV)
        {
            if (descriptor->register_index > VKD3D_SHADER_D3DBC_BOOL_CONSTANT_REGISTER)
            {
                snprintf(wb_messages, sizeof(wb_messages), "Unsupported legacy constant register set");
                return 0;
            }
            binding->flags = VKD3D_SHADER_BINDING_FLAG_BUFFER;
            binding->binding.binding = descriptor->register_index;
        }
        else
        {
            if (descriptor->register_index > 15)
            {
                snprintf(wb_messages, sizeof(wb_messages), "Legacy sampler index exceeds 15");
                return 0;
            }
            binding->flags = descriptor->type == VKD3D_SHADER_DESCRIPTOR_TYPE_SRV
                    ? VKD3D_SHADER_BINDING_FLAG_IMAGE : 0;
            binding->binding.binding = 16 + 2 * descriptor->register_index
                    + (descriptor->type == VKD3D_SHADER_DESCRIPTOR_TYPE_SAMPLER);
        }
    }

    interface.type = VKD3D_SHADER_STRUCTURE_TYPE_INTERFACE_INFO;
    interface.next = varying_map;
    interface.bindings = bindings;
    interface.binding_count = wb_legacy_descriptors[stage].descriptor_count;
    source.type = VKD3D_SHADER_STRUCTURE_TYPE_D3DBC_SOURCE_INFO;
    source.next = &interface;
    target.type = VKD3D_SHADER_STRUCTURE_TYPE_SPIRV_TARGET_INFO;
    target.next = &source;
    target.entry_point = "main";
    target.environment = VKD3D_SHADER_SPIRV_ENVIRONMENT_VULKAN_1_0;
    info.type = VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO;
    info.next = &target;
    info.source.code = bytes;
    info.source.size = length;
    info.source_type = VKD3D_SHADER_SOURCE_D3D_BYTECODE;
    info.target_type = VKD3D_SHADER_TARGET_SPIRV_BINARY;
    info.options = options;
    info.option_count = sizeof(options) / sizeof(options[0]);
    info.log_level = VKD3D_SHADER_LOG_WARNING;
    result = vkd3d_shader_compile(&info, &wb_legacy_results[stage], &messages);
    wb_capture_messages(messages);
    if (result < 0 || !wb_legacy_results[stage].code || wb_legacy_results[stage].size < 20
            || wb_legacy_results[stage].size > WB_MAX_SPIRV || wb_legacy_results[stage].size % 4)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "Legacy shader compile failed (%d)", result);
        vkd3d_shader_free_shader_code(&wb_legacy_results[stage]);
        memset(&wb_legacy_results[stage], 0, sizeof(wb_legacy_results[stage]));
        return 0;
    }
    return 1;
}

int wb_d3dbc_compile_pair(const void *vertex, unsigned int vertex_length,
        const void *pixel, unsigned int pixel_length)
{
    struct vkd3d_shader_varying_map varyings[WB_MAX_LEGACY_VARYINGS];
    struct vkd3d_shader_varying_map_info varying_info = {0};
    unsigned int varying_count = WB_MAX_LEGACY_VARYINGS;

    wb_clear();
    if (!wb_legacy_validate(0, vertex, vertex_length) || !wb_legacy_validate(1, pixel, pixel_length)
            || !wb_legacy_scan(0, vertex, vertex_length) || !wb_legacy_scan(1, pixel, pixel_length))
        return 0;
    if (wb_legacy_signatures[1].input.element_count > WB_MAX_LEGACY_VARYINGS)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Legacy pixel varying count exceeds %u", WB_MAX_LEGACY_VARYINGS);
        return 0;
    }
    vkd3d_shader_build_varying_map(&wb_legacy_signatures[0].output,
            &wb_legacy_signatures[1].input, &varying_count, varyings);
    if (varying_count > WB_MAX_LEGACY_VARYINGS)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Legacy varying map exceeds %u", WB_MAX_LEGACY_VARYINGS);
        return 0;
    }
    varying_info.type = VKD3D_SHADER_STRUCTURE_TYPE_VARYING_MAP_INFO;
    varying_info.varying_map = varyings;
    varying_info.varying_count = varying_count;
    return wb_legacy_compile_stage(0, vertex, vertex_length, &varying_info)
            && wb_legacy_compile_stage(1, pixel, pixel_length, NULL);
}

const void *wb_d3dbc_result_ptr(unsigned int stage)
{
    return stage < WB_LEGACY_STAGES ? wb_legacy_results[stage].code : NULL;
}

unsigned int wb_d3dbc_result_size(unsigned int stage)
{
    return stage < WB_LEGACY_STAGES ? (unsigned int)wb_legacy_results[stage].size : 0;
}

/* Descriptor scanning and explicit target bindings for SM4/SM5 DXBC.
 *
 * vkd3d-shader assigns target bindings itself when no interface is supplied:
 * every variable lands in set 0 with sequential binding indices ordered by
 * first use. That order is not knowable before compilation, so a D3D12 root
 * signature cannot be mapped onto it. These entry points first scan a shader
 * for its declared D3D registers, then compile it again with an explicit
 * (register -> set/binding) table so the caller controls the layout.
 *
 * Entry layout, 7 uint32 per record, shared by both calls:
 *   0 type              vkd3d_shader_descriptor_type
 *   1 register_space    HLSL register space
 *   2 register_index    HLSL register number
 *   3 resource_type     vkd3d_shader_resource_type (scan only; drives flags)
 *   4 resource_data_type / target set
 *   5 flags             / target binding
 *   6 count             descriptor array length (1 when not an array)
 */
/* Parsed root-signature shape, flattened into a single word array so the
 * caller can build a canonical binding table without re-parsing the blob.
 *
 * Header, 6 words:
 *   0 parameter count          1 static sampler count
 *   2 flags                    3 root constants total (DWORDs)
 *   4 static sampler word count (record width x count)
 *   5 reserved
 * Parameters, 7 words each:
 *   0 parameter type   1 visibility   2 descriptor range count
 *   3 shader register  4 register space   5 value count / ignored
 *   6 range record offset (relative to range base)
 * Ranges, 5 words each:
 *   0 range type   1 descriptor count   2 base shader register
 *   3 register space   4 table offset
 * Static samplers, 11 words each:
 *   0 filter   1-3 address u/v/w   4 mip lod bias (float bits)
 *   5 max anisotropy   6 comparison func   7 border colour
 *   8 min lod   9 max lod   10 (register | space << 16 | visibility << 24)
 */
int wb_dxbc_scan(const void *bytes, unsigned int length)
{
    struct vkd3d_shader_scan_descriptor_info descriptor_info = {0};
    struct vkd3d_shader_compile_info info = {0};
    uint32_t magic = 0;
    char *messages = NULL;
    unsigned int i;
    int result;

    wb_messages[0] = '\0';
    wb_descriptor_count = 0;
    if (!bytes || length < 32 || length > WB_MAX_DXBC)
    {
        snprintf(wb_messages, sizeof(wb_messages), "DXBC scan length must be 32..%u bytes", WB_MAX_DXBC);
        return 0;
    }
    memcpy(&magic, bytes, sizeof(magic));
    if (magic != 0x43425844u)
    {
        snprintf(wb_messages, sizeof(wb_messages), "DXBC container signature is missing");
        return 0;
    }

    descriptor_info.type = VKD3D_SHADER_STRUCTURE_TYPE_SCAN_DESCRIPTOR_INFO;
    info.type = VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO;
    info.next = &descriptor_info;
    info.source.code = bytes;
    info.source.size = length;
    info.source_type = VKD3D_SHADER_SOURCE_DXBC_TPF;
    info.target_type = VKD3D_SHADER_TARGET_SPIRV_BINARY;
    info.log_level = VKD3D_SHADER_LOG_WARNING;
    result = vkd3d_shader_scan(&info, &messages);
    wb_capture_messages(messages);
    if (result < 0)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "DXBC descriptor scan failed (%d)", result);
        return 0;
    }
    if (descriptor_info.descriptor_count > WB_MAX_SCAN_DESCRIPTORS)
    {
        snprintf(wb_messages, sizeof(wb_messages), "DXBC descriptor count exceeds %u", WB_MAX_SCAN_DESCRIPTORS);
        vkd3d_shader_free_scan_descriptor_info(&descriptor_info);
        return 0;
    }
    for (i = 0; i < descriptor_info.descriptor_count; ++i)
    {
        const struct vkd3d_shader_descriptor_info *descriptor = &descriptor_info.descriptors[i];
        uint32_t *record = &wb_descriptor_results[i * WB_DESCRIPTOR_RECORD];

        record[0] = descriptor->type;
        record[1] = descriptor->register_space;
        record[2] = descriptor->register_index;
        record[3] = descriptor->resource_type;
        record[4] = descriptor->resource_data_type;
        record[5] = descriptor->flags;
        record[6] = descriptor->count;
    }
    wb_descriptor_count = descriptor_info.descriptor_count;
    vkd3d_shader_free_scan_descriptor_info(&descriptor_info);
    return 1;
}

const uint32_t *wb_dxbc_scan_results(void) { return wb_descriptor_results; }
unsigned int wb_dxbc_scan_count(void) { return wb_descriptor_count; }

static unsigned int wb_binding_flag_for_resource_type(unsigned int resource_type)
{
    if (resource_type == VKD3D_SHADER_RESOURCE_BUFFER)
        return VKD3D_SHADER_BINDING_FLAG_BUFFER;
    if (resource_type == VKD3D_SHADER_RESOURCE_NONE)
        return 0;
    return VKD3D_SHADER_BINDING_FLAG_IMAGE;
}

int wb_dxbc_compile_bound(const void *bytes, unsigned int length,
        const uint32_t *records, unsigned int record_count)
{
    const struct vkd3d_shader_compile_option options[] =
    {
        {VKD3D_SHADER_COMPILE_OPTION_WRITE_TESS_GEOM_POINT_SIZE, 0},
    };
    struct vkd3d_shader_resource_binding bindings[WB_MAX_SCAN_DESCRIPTORS];
    struct vkd3d_shader_interface_info interface = {0};
    struct vkd3d_shader_spirv_target_info target = {0};
    struct vkd3d_shader_compile_info info = {0};
    uint32_t magic = 0;
    char *messages = NULL;
    unsigned int i;
    int result;

    wb_clear();
    if (!bytes || length < 32 || length > WB_MAX_DXBC)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Bound DXBC length must be 32..%u bytes", WB_MAX_DXBC);
        return 0;
    }
    if (record_count > WB_MAX_SCAN_DESCRIPTORS)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Bound descriptor count exceeds %u", WB_MAX_SCAN_DESCRIPTORS);
        return 0;
    }
    memcpy(&magic, bytes, sizeof(magic));
    if (magic != 0x43425844u)
    {
        snprintf(wb_messages, sizeof(wb_messages), "DXBC container signature is missing");
        return 0;
    }
    for (i = 0; i < record_count; ++i)
    {
        const uint32_t *record = &records[i * WB_DESCRIPTOR_RECORD];
        struct vkd3d_shader_resource_binding *binding = &bindings[i];

        if (record[0] > VKD3D_SHADER_DESCRIPTOR_TYPE_SAMPLER || !record[6])
        {
            snprintf(wb_messages, sizeof(wb_messages), "Bound descriptor record %u is invalid", i);
            return 0;
        }
        memset(binding, 0, sizeof(*binding));
        binding->type = record[0];
        binding->register_space = record[1];
        binding->register_index = record[2];
        binding->shader_visibility = VKD3D_SHADER_VISIBILITY_ALL;
        binding->flags = wb_binding_flag_for_resource_type(record[3]);
        binding->binding.set = record[4];
        binding->binding.binding = record[5];
        binding->binding.count = record[6];
    }

    interface.type = VKD3D_SHADER_STRUCTURE_TYPE_INTERFACE_INFO;
    interface.bindings = record_count ? bindings : NULL;
    interface.binding_count = record_count;
    target.type = VKD3D_SHADER_STRUCTURE_TYPE_SPIRV_TARGET_INFO;
    target.next = &interface;
    target.entry_point = "main";
    target.environment = VKD3D_SHADER_SPIRV_ENVIRONMENT_VULKAN_1_0;
    info.type = VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO;
    info.next = &target;
    info.source.code = bytes;
    info.source.size = length;
    info.source_type = VKD3D_SHADER_SOURCE_DXBC_TPF;
    info.target_type = VKD3D_SHADER_TARGET_SPIRV_BINARY;
    info.options = options;
    info.option_count = sizeof(options) / sizeof(options[0]);
    info.log_level = VKD3D_SHADER_LOG_WARNING;
    result = vkd3d_shader_compile(&info, &wb_result, &messages);
    wb_capture_messages(messages);
    if (result < 0 || !wb_result.code || wb_result.size < 20 || wb_result.size > WB_MAX_SPIRV || wb_result.size % 4)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "Bound DXBC compile failed (%d)", result);
        vkd3d_shader_free_shader_code(&wb_result);
        memset(&wb_result, 0, sizeof(wb_result));
        return 0;
    }
    return 1;
}

/* Compile application-provided HLSL in the browser, without shader substitution. */
int wb_hlsl_compile(const void *bytes, unsigned int length, const char *entry,
        const char *profile, const char *source_name)
{
    struct vkd3d_shader_hlsl_source_info hlsl = {0};
    struct vkd3d_shader_compile_info info = {0};
    char *messages = NULL;
    int result;

    wb_clear();
    /* The profile is a shader-model request the library resolves itself, so the
     * guard only has to reject an empty or absurd word. A hard-coded pair here
     * refused every D3D10 profile even though vkd3d-shader compiles vs_4_0 and
     * ps_4_0 (and their 4_1/level_9 variants) through the same path. */
    if (!bytes || !length || length > WB_MAX_DXBC || !entry || !entry[0] || !profile
            || !*profile || strnlen(profile, 64) >= 64)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Expected bounded HLSL and an entry point with a shader profile");
        return 0;
    }
    hlsl.type = VKD3D_SHADER_STRUCTURE_TYPE_HLSL_SOURCE_INFO;
    hlsl.entry_point = entry;
    hlsl.profile = profile;
    info.type = VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO;
    info.next = &hlsl;
    info.source.code = bytes;
    info.source.size = length;
    info.source_type = VKD3D_SHADER_SOURCE_HLSL;
    info.target_type = VKD3D_SHADER_TARGET_DXBC_TPF;
    info.source_name = source_name;
    info.log_level = VKD3D_SHADER_LOG_WARNING;
    result = vkd3d_shader_compile(&info, &wb_result, &messages);
    wb_capture_messages(messages);
    if (result < 0 || !wb_result.code || wb_result.size < 32 || wb_result.size > WB_MAX_DXBC)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "HLSL compilation failed (%d)", result);
        vkd3d_shader_free_shader_code(&wb_result);
        memset(&wb_result, 0, sizeof(wb_result));
        return 0;
    }
    return 1;
}

int wb_dxbc_compile(const void *bytes, unsigned int length)
{
    const struct vkd3d_shader_compile_option options[] =
    {
        {VKD3D_SHADER_COMPILE_OPTION_WRITE_TESS_GEOM_POINT_SIZE, 0},
    };
    struct vkd3d_shader_spirv_target_info target = {0};
    struct vkd3d_shader_compile_info info = {0};
    uint32_t magic = 0;
    char *messages = NULL;
    int result;

    wb_clear();
    if (!bytes || length < 32 || length > WB_MAX_DXBC)
    {
        snprintf(wb_messages, sizeof(wb_messages), "DXBC input length must be 32..%u bytes", WB_MAX_DXBC);
        return 0;
    }
    memcpy(&magic, bytes, sizeof(magic));
    if (magic != 0x43425844u)
    {
        snprintf(wb_messages, sizeof(wb_messages), "DXBC container signature is missing");
        return 0;
    }

    target.type = VKD3D_SHADER_STRUCTURE_TYPE_SPIRV_TARGET_INFO;
    target.entry_point = "main";
    target.environment = VKD3D_SHADER_SPIRV_ENVIRONMENT_VULKAN_1_0;
    info.type = VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO;
    info.next = &target;
    info.source.code = bytes;
    info.source.size = length;
    info.source_type = VKD3D_SHADER_SOURCE_DXBC_TPF;
    info.target_type = VKD3D_SHADER_TARGET_SPIRV_BINARY;
    info.options = options;
    info.option_count = sizeof(options) / sizeof(options[0]);
    info.log_level = VKD3D_SHADER_LOG_WARNING;
    result = vkd3d_shader_compile(&info, &wb_result, &messages);
    wb_capture_messages(messages);
    if (result < 0 || !wb_result.code || wb_result.size < 20 || wb_result.size > WB_MAX_SPIRV || wb_result.size % 4)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "vkd3d-shader compile failed (%d)", result);
        vkd3d_shader_free_shader_code(&wb_result);
        memset(&wb_result, 0, sizeof(wb_result));
        return 0;
    }
    return 1;
}

/* A real DXBC root-signature container, restricted to the empty v1.0 shape
 * needed by the first D3D12 triangle. Unsupported shapes fail explicitly. */
int wb_root_signature_serialize(unsigned int flags)
{
    struct vkd3d_shader_versioned_root_signature_desc desc = {0};
    char *messages = NULL;
    int result;

    wb_clear();
    if (flags & ~VKD3D_SHADER_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Unsupported root signature flags %#x", flags);
        return 0;
    }
    desc.version = VKD3D_SHADER_ROOT_SIGNATURE_VERSION_1_0;
    desc.u.v_1_0.flags = (enum vkd3d_shader_root_signature_flags)flags;
    result = vkd3d_shader_serialize_root_signature(&desc, &wb_result, &messages);
    wb_capture_messages(messages);
    if (result < 0 || !wb_result.code || wb_result.size < 32 || wb_result.size > WB_MAX_DXBC)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "Root signature serialization failed (%d)", result);
        vkd3d_shader_free_shader_code(&wb_result);
        memset(&wb_result, 0, sizeof(wb_result));
        return 0;
    }
    wb_root_flags = flags;
    return 1;
}

int wb_root_signature_validate(const void *bytes, unsigned int length)
{
    struct vkd3d_shader_versioned_root_signature_desc desc = {0};
    struct vkd3d_shader_code dxbc;
    char *messages = NULL;
    uint32_t magic = 0;
    int result;

    /* Keep wb_result alive: bytes may point to the previous serialize result. */
    wb_messages[0] = '\0';
    wb_root_flags = 0;
    if (!bytes || length < 32 || length > WB_MAX_DXBC)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Root signature length must be 32..%u bytes", WB_MAX_DXBC);
        return 0;
    }
    memcpy(&magic, bytes, sizeof(magic));
    if (magic != 0x43425844u)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Root signature DXBC container signature is missing");
        return 0;
    }
    dxbc.code = bytes;
    dxbc.size = length;
    result = vkd3d_shader_parse_root_signature(&dxbc, &desc, &messages);
    wb_capture_messages(messages);
    if (result < 0)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "Root signature parse failed (%d)", result);
        return 0; /* vkd3d frees partial descriptions on parse failure. */
    }
    if (desc.version != VKD3D_SHADER_ROOT_SIGNATURE_VERSION_1_0
            || desc.u.v_1_0.parameter_count || desc.u.v_1_0.static_sampler_count
            || (desc.u.v_1_0.flags & ~VKD3D_SHADER_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT))
    {
        snprintf(wb_messages, sizeof(wb_messages), "Only empty version 1.0 root signatures with flags 0 or 1 are supported");
        vkd3d_shader_free_root_signature(&desc);
        return 0;
    }
    wb_root_flags = desc.u.v_1_0.flags;
    vkd3d_shader_free_root_signature(&desc);
    return 1;
}

unsigned int wb_root_signature_flags(void) { return wb_root_flags; }

/* Parse a serialized root signature and expose its structure. The empty
 * version 1.0 signature has always been accepted; this additionally reports
 * parameters, descriptor ranges and static samplers so the backend can map a
 * shader's HLSL registers onto canonical, signature-derived bindings. */
int wb_root_signature_inspect(const void *bytes, unsigned int length)
{
    struct vkd3d_shader_versioned_root_signature_desc desc = {0};
    struct vkd3d_shader_code dxbc;
    char *messages = NULL;
    uint32_t magic = 0;
    unsigned int parameter_index, range_index, sampler_index, cursor;
    unsigned int total_ranges = 0, total_constants = 0;
    int result;

    wb_messages[0] = '\0';
    wb_root_flags = 0;
    wb_root_word_count = 0;
    if (!bytes || length < 32 || length > WB_MAX_DXBC)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Root signature length must be 32..%u bytes", WB_MAX_DXBC);
        return 0;
    }
    memcpy(&magic, bytes, sizeof(magic));
    if (magic != 0x43425844u)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Root signature DXBC container signature is missing");
        return 0;
    }
    dxbc.code = bytes;
    dxbc.size = length;
    result = vkd3d_shader_parse_root_signature(&dxbc, &desc, &messages);
    wb_capture_messages(messages);
    if (result < 0)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "Root signature parse failed (%d)", result);
        return 0;
    }
    if (desc.version != VKD3D_SHADER_ROOT_SIGNATURE_VERSION_1_0)
        goto unsupported;

    for (parameter_index = 0; parameter_index < desc.u.v_1_0.parameter_count; ++parameter_index)
    {
        const struct vkd3d_shader_root_parameter *parameter = &desc.u.v_1_0.parameters[parameter_index];
        if (parameter->parameter_type == VKD3D_SHADER_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS)
            total_constants += parameter->u.constants.value_count;
        else if (parameter->parameter_type == VKD3D_SHADER_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE)
            total_ranges += parameter->u.descriptor_table.descriptor_range_count;
    }
    if (desc.u.v_1_0.parameter_count > WB_MAX_ROOT_PARAMETERS || total_ranges > WB_MAX_ROOT_RANGES
            || desc.u.v_1_0.static_sampler_count > WB_MAX_ROOT_SAMPLERS)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Root signature exceeds the inspection limits");
        goto unsupported;
    }

    cursor = WB_ROOT_HEADER_WORDS;
    /* Ranges are appended in parameter order starting after the whole
     * parameter block, so each parameter's range base is a running offset
     * from the first range slot rather than a fixed distance from the
     * parameter record. */
    range_index = WB_ROOT_HEADER_WORDS
            + desc.u.v_1_0.parameter_count * WB_ROOT_PARAMETER_WORDS;
    for (parameter_index = 0; parameter_index < desc.u.v_1_0.parameter_count; ++parameter_index)
    {
        const struct vkd3d_shader_root_parameter *parameter = &desc.u.v_1_0.parameters[parameter_index];
        uint32_t *record = &wb_root_words[cursor];

        record[0] = parameter->parameter_type;
        record[1] = parameter->shader_visibility;
        record[2] = 0;
        record[3] = 0;
        record[4] = 0;
        record[5] = 0;
        record[6] = 0;
        if (parameter->parameter_type == VKD3D_SHADER_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE)
        {
            const struct vkd3d_shader_root_descriptor_table *table = &parameter->u.descriptor_table;
            record[2] = table->descriptor_range_count;
            record[6] = range_index; /* first range slot for this parameter */
            range_index += table->descriptor_range_count * WB_ROOT_RANGE_WORDS;
        }
        else if (parameter->parameter_type == VKD3D_SHADER_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS)
        {
            record[3] = parameter->u.constants.shader_register;
            record[4] = parameter->u.constants.register_space;
            record[5] = parameter->u.constants.value_count;
        }
        else
        {
            record[3] = parameter->u.descriptor.shader_register;
            record[4] = parameter->u.descriptor.register_space;
        }
        cursor += WB_ROOT_PARAMETER_WORDS;
    }
    /* Ranges follow the parameter block; the offsets written above point at
     * the first range slot for each descriptor-table parameter. */
    range_index = WB_ROOT_HEADER_WORDS
            + desc.u.v_1_0.parameter_count * WB_ROOT_PARAMETER_WORDS;
    for (parameter_index = 0; parameter_index < desc.u.v_1_0.parameter_count; ++parameter_index)
    {
        const struct vkd3d_shader_root_parameter *parameter = &desc.u.v_1_0.parameters[parameter_index];
        unsigned int r;
        if (parameter->parameter_type != VKD3D_SHADER_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE)
            continue;
        for (r = 0; r < parameter->u.descriptor_table.descriptor_range_count; ++r)
        {
            const struct vkd3d_shader_descriptor_range *range = &parameter->u.descriptor_table.descriptor_ranges[r];
            uint32_t *record = &wb_root_words[range_index];
            record[0] = range->range_type;
            record[1] = range->descriptor_count;
            record[2] = range->base_shader_register;
            record[3] = range->register_space;
            record[4] = range->descriptor_table_offset;
            range_index += WB_ROOT_RANGE_WORDS;
        }
    }
    for (sampler_index = 0; sampler_index < desc.u.v_1_0.static_sampler_count; ++sampler_index)
    {
        const struct vkd3d_shader_static_sampler_desc *sampler = &desc.u.v_1_0.static_samplers[sampler_index];
        uint32_t *record = &wb_root_words[range_index];
        float bias = sampler->mip_lod_bias, min_lod = sampler->min_lod, max_lod = sampler->max_lod;

        record[0] = sampler->filter;
        record[1] = sampler->address_u;
        record[2] = sampler->address_v;
        record[3] = sampler->address_w;
        memcpy(&record[4], &bias, sizeof(bias));
        record[5] = sampler->max_anisotropy;
        record[6] = sampler->comparison_func;
        record[7] = sampler->border_colour;
        memcpy(&record[8], &min_lod, sizeof(min_lod));
        memcpy(&record[9], &max_lod, sizeof(max_lod));
        record[10] = (sampler->shader_register & 0xffffu)
                | ((sampler->register_space & 0xffu) << 16)
                | ((sampler->shader_visibility & 0xffu) << 24);
        range_index += WB_ROOT_SAMPLER_WORDS;
    }

    wb_root_words[0] = desc.u.v_1_0.parameter_count;
    wb_root_words[1] = desc.u.v_1_0.static_sampler_count;
    wb_root_words[2] = desc.u.v_1_0.flags;
    wb_root_words[3] = total_constants;
    wb_root_words[4] = WB_ROOT_SAMPLER_WORDS;
    wb_root_words[5] = 0;
    wb_root_word_count = range_index;
    wb_root_flags = desc.u.v_1_0.flags;
    vkd3d_shader_free_root_signature(&desc);
    return 1;

unsupported:
    snprintf(wb_messages, sizeof(wb_messages),
            "Root signature version/structure is outside the bounded inspection subset");
    vkd3d_shader_free_root_signature(&desc);
    return 0;
}

/* Build and serialize a root signature from the same flattened word layout
 * that wb_root_signature_inspect emits. D3D12SerializeRootSignature takes a
 * guest D3D12_ROOT_SIGNATURE_DESC, which the browser frontend flattens; this
 * turns that description into the real DXBC container vkd3d-shader produces,
 * so an application-declared signature is honoured rather than replaced.
 *
 * Ranges are indexed by a single running cursor in declaration order, and each
 * descriptor-table parameter stores the absolute index of its first range. */
int wb_root_signature_build(const uint32_t *words, unsigned int word_count)
{
    struct vkd3d_shader_root_parameter parameters[WB_MAX_ROOT_PARAMETERS];
    struct vkd3d_shader_descriptor_range ranges[WB_MAX_ROOT_RANGES];
    struct vkd3d_shader_static_sampler_desc samplers[WB_MAX_ROOT_SAMPLERS];
    struct vkd3d_shader_versioned_root_signature_desc desc;
    unsigned int parameter_count, sampler_count, range_cursor, sampler_start;
    unsigned int i, r, range_total = 0, sampler_total = 0;
    char *messages = NULL;
    int result;

    wb_clear();
    if (!words || word_count < WB_ROOT_HEADER_WORDS)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Root signature description is truncated");
        return 0;
    }
    parameter_count = words[0];
    sampler_count = words[1];
    if (parameter_count > WB_MAX_ROOT_PARAMETERS || sampler_count > WB_MAX_ROOT_SAMPLERS)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Root signature description exceeds the build limits");
        return 0;
    }
    memset(parameters, 0, sizeof(parameters));
    memset(ranges, 0, sizeof(ranges));
    memset(samplers, 0, sizeof(samplers));
    memset(&desc, 0, sizeof(desc));

    range_cursor = WB_ROOT_HEADER_WORDS + parameter_count * WB_ROOT_PARAMETER_WORDS;
    sampler_start = range_cursor; /* recomputed after the parameter walk */
    for (i = 0; i < parameter_count; ++i)
    {
        const uint32_t *record = &words[WB_ROOT_HEADER_WORDS + i * WB_ROOT_PARAMETER_WORDS];
        struct vkd3d_shader_root_parameter *parameter = &parameters[i];

        if (record[0] > VKD3D_SHADER_ROOT_PARAMETER_TYPE_UAV)
        {
            snprintf(wb_messages, sizeof(wb_messages), "Root parameter %u has an unknown type", i);
            return 0;
        }
        if (record[1] > VKD3D_SHADER_VISIBILITY_PIXEL)
        {
            snprintf(wb_messages, sizeof(wb_messages), "Root parameter %u has an unknown visibility", i);
            return 0;
        }
        parameter->parameter_type = record[0];
        parameter->shader_visibility = record[1];
        if (record[0] == VKD3D_SHADER_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE)
        {
            if (range_total + record[2] > WB_MAX_ROOT_RANGES)
            {
                snprintf(wb_messages, sizeof(wb_messages), "Descriptor tables exceed the build limits");
                return 0;
            }
            parameter->u.descriptor_table.descriptor_range_count = record[2];
            parameter->u.descriptor_table.descriptor_ranges = &ranges[range_total];
            range_total += record[2];
        }
        else if (record[0] == VKD3D_SHADER_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS)
        {
            if (!record[5] || record[5] > 64)
            {
                snprintf(wb_messages, sizeof(wb_messages), "Root constants %u must hold 1..64 DWORDs", i);
                return 0;
            }
            parameter->u.constants.shader_register = record[3];
            parameter->u.constants.register_space = record[4];
            parameter->u.constants.value_count = record[5];
        }
        else
        {
            parameter->u.descriptor.shader_register = record[3];
            parameter->u.descriptor.register_space = record[4];
        }
    }

    sampler_start = range_cursor + range_total * WB_ROOT_RANGE_WORDS;
    if (word_count < sampler_start + sampler_count * WB_ROOT_SAMPLER_WORDS)
    {
        snprintf(wb_messages, sizeof(wb_messages), "Root signature range or sampler block is truncated");
        return 0;
    }

    range_total = 0;
    for (i = 0; i < parameter_count; ++i)
    {
        const uint32_t *record = &words[WB_ROOT_HEADER_WORDS + i * WB_ROOT_PARAMETER_WORDS];
        if (record[0] != VKD3D_SHADER_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE)
            continue;
        for (r = 0; r < record[2]; ++r)
        {
            const uint32_t *source = &words[range_cursor + r * WB_ROOT_RANGE_WORDS];
            struct vkd3d_shader_descriptor_range *range = &ranges[range_total++];
            if (source[0] > VKD3D_SHADER_DESCRIPTOR_TYPE_SAMPLER || !source[1])
            {
                snprintf(wb_messages, sizeof(wb_messages), "Descriptor range is invalid");
                return 0;
            }
            range->range_type = source[0];
            range->descriptor_count = source[1];
            range->base_shader_register = source[2];
            range->register_space = source[3];
            range->descriptor_table_offset = source[4];
        }
        range_cursor += record[2] * WB_ROOT_RANGE_WORDS;
    }

    for (i = 0; i < sampler_count; ++i)
    {
        const uint32_t *record = &words[sampler_start + i * WB_ROOT_SAMPLER_WORDS];
        float bias, min_lod, max_lod;
        uint32_t packed = record[10];

        memcpy(&bias, &record[4], sizeof(bias));
        memcpy(&min_lod, &record[8], sizeof(min_lod));
        memcpy(&max_lod, &record[9], sizeof(max_lod));
        samplers[i].filter = record[0];
        samplers[i].address_u = record[1];
        samplers[i].address_v = record[2];
        samplers[i].address_w = record[3];
        samplers[i].mip_lod_bias = bias;
        samplers[i].max_anisotropy = record[5];
        samplers[i].comparison_func = record[6];
        samplers[i].border_colour = record[7];
        samplers[i].min_lod = min_lod;
        samplers[i].max_lod = max_lod;
        samplers[i].shader_register = packed & 0xffffu;
        samplers[i].register_space = (packed >> 16) & 0xffu;
        samplers[i].shader_visibility = (packed >> 24) & 0xffu;
        if (samplers[i].shader_visibility > VKD3D_SHADER_VISIBILITY_PIXEL)
        {
            snprintf(wb_messages, sizeof(wb_messages), "Static sampler %u has an unknown visibility", i);
            return 0;
        }
    }
    sampler_total = sampler_count;

    desc.version = VKD3D_SHADER_ROOT_SIGNATURE_VERSION_1_0;
    desc.u.v_1_0.parameter_count = parameter_count;
    desc.u.v_1_0.parameters = parameter_count ? parameters : NULL;
    desc.u.v_1_0.static_sampler_count = sampler_total;
    desc.u.v_1_0.static_samplers = sampler_total ? samplers : NULL;
    desc.u.v_1_0.flags = words[2];
    result = vkd3d_shader_serialize_root_signature(&desc, &wb_result, &messages);
    wb_capture_messages(messages);
    if (result < 0 || !wb_result.code || wb_result.size < 32 || wb_result.size > WB_MAX_DXBC)
    {
        if (!wb_messages[0]) snprintf(wb_messages, sizeof(wb_messages), "Root signature serialization failed (%d)", result);
        vkd3d_shader_free_shader_code(&wb_result);
        memset(&wb_result, 0, sizeof(wb_result));
        return 0;
    }
    wb_root_flags = words[2];
    return 1;
}

const uint32_t *wb_root_signature_words(void) { return wb_root_words; }
unsigned int wb_root_signature_word_count(void) { return wb_root_word_count; }

const void *wb_result_ptr(void) { return wb_result.code; }
unsigned int wb_result_size(void) { return (unsigned int)wb_result.size; }
const char *wb_messages_ptr(void) { return wb_messages; }
