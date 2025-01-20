struct RayDesc_ {
    uint flags;
    uint cull_mask;
    float tmin;
    float tmax;
    float3 origin;
    int _pad5_0;
    float3 dir;
    int _end_pad_0;
};


struct Container {
    uint inner;
};

RayDesc RayDescFromRayDesc_(RayDesc_ arg0) {
    RayDesc ret = (RayDesc)0;
    ret.Origin = arg0.origin;
    ret.TMin = arg0.tmin;
    ret.Direction = arg0.dir;
    ret.TMax = arg0.tmax;
    return ret;
}

RaytracingAccelerationStructure acc_struct : register(t0);

float4 ZeroValuefloat4() {
    return (float4)0;
}

RayDesc_ ZeroValueRayDesc_() {
    return (RayDesc_)0;
}

void trace()
{
    float4 colour_5 = ZeroValuefloat4();

    NagaWrapperStructForfloat4 NagaWrapperStructForfloat4Temp = NagaWrapperStructForfloat4Construct(colour_5);
    TraceRay(acc_struct, ZeroValueRayDesc_().flags, ZeroValueRayDesc_().cull_mask, 0, 0, 0, RayDescFromRayDesc_(ZeroValueRayDesc_()), NagaWrapperStructForfloat4Temp);
    return;
}

struct NagaWrapperStructForfloat4 {
float4 inner;
};
NagaWrapperStructForfloat4 NagaWrapperStructForfloat4Construct(float4 inner) {
    NagaWrapperStructForfloat4 ret = (NagaWrapperStructForfloat4)0;
    ret.inner = inner;
    return ret;
}
[shader("raygeneration")]
void ray_gen()
{
    float4 colour = ZeroValuefloat4();

    NagaWrapperStructForfloat4 NagaWrapperStructForfloat4Temp = NagaWrapperStructForfloat4Construct(colour);
    TraceRay(acc_struct, ZeroValueRayDesc_().flags, ZeroValueRayDesc_().cull_mask, 0, 0, 0, RayDescFromRayDesc_(ZeroValueRayDesc_()), NagaWrapperStructForfloat4Temp);
    trace();
    return;
}

[shader("anyhit")]
void discard_any_hit(inout NagaWrapperStructForfloat4 naga_payload_wrapped, in BuiltInTriangleIntersectionAttributes intersection)
{
    float4 colour_1 = naga_payload_wrapped.inner;
    colour_1 = ZeroValuefloat4();
    naga_payload_wrapped.inner = colour_1;
    discard;
}

[shader("anyhit")]
void any_hit(inout NagaWrapperStructForfloat4 naga_payload_wrapped, in BuiltInTriangleIntersectionAttributes intersection_1)
{
    float4 colour_2 = naga_payload_wrapped.inner;
    float t = RayTCurrent();
    colour_2 = ZeroValuefloat4();
    naga_payload_wrapped.inner = colour_2;
    return;
}

[shader("closesthit")]
void closest_hit(inout NagaWrapperStructForfloat4 naga_payload_wrapped, in BuiltInTriangleIntersectionAttributes intersection_2)
{
    float4 colour_3 = naga_payload_wrapped.inner;
    float t_1 = RayTCurrent();
    colour_3 = (1.0).xxxx;
    naga_payload_wrapped.inner = colour_3;
    return;
}

[shader("miss")]
void miss(inout NagaWrapperStructForfloat4 naga_payload_wrapped)
{
    float4 colour_4 = naga_payload_wrapped.inner;
    colour_4 = ZeroValuefloat4();
    naga_payload_wrapped.inner = colour_4;
    return;
}

struct NagaWrapperStructForuint {
uint inner;
};
NagaWrapperStructForuint NagaWrapperStructForuintConstruct(uint inner) {
    NagaWrapperStructForuint ret = (NagaWrapperStructForuint)0;
    ret.inner = inner;
    return ret;
}
[shader("intersection")]
void intersect_return()
{
    ReportHit(0.5, 5u,NagaWrapperStructForuintConstruct(0u));
    return;
}

Container ConstructContainer(uint arg0) {
    Container ret = (Container)0;
    ret.inner = arg0;
    return ret;
}

[shader("intersection")]
void intersect_struct()
{
    ReportHit(0.5, 5u,ConstructContainer(0u));
    return;
}
