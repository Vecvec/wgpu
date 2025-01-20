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

TraceRay(acc_struct, ZeroValueRayDesc_().flags, ZeroValueRayDesc_().cull_mask, 0, 0, 0, RayDescFromRayDesc_(ZeroValueRayDesc_()), colour_5);    return;
}

[shader("raygeneration")]
void ray_gen()
{
    float4 colour = ZeroValuefloat4();

TraceRay(acc_struct, ZeroValueRayDesc_().flags, ZeroValueRayDesc_().cull_mask, 0, 0, 0, RayDescFromRayDesc_(ZeroValueRayDesc_()), colour);    trace();
    return;
}

[shader("anyhit")]
void discard_any_hit(inout float4 colour_1, in BuiltInTriangleIntersectionAttributes intersection)
{
    colour_1 = ZeroValuefloat4();
    discard;
}

[shader("anyhit")]
void any_hit(inout float4 colour_2, in BuiltInTriangleIntersectionAttributes intersection_1)
{
float t = RayTCurrent();
    colour_2 = ZeroValuefloat4();
    return;
}

[shader("closesthit")]
void closest_hit(inout float4 colour_3, in BuiltInTriangleIntersectionAttributes intersection_2)
{
float t_1 = RayTCurrent();
    colour_3 = (1.0).xxxx;
    return;
}

[shader("miss")]
void miss(inout float4 colour_4)
{
    colour_4 = ZeroValuefloat4();
    return;
}

struct NagaWrapperStructFor3 {
uint inner,};
NagaWrapperStructFor3 NagaWrapperStructFor3Construct(uint inner) {NagaWrapperStructFor3 ret = (NagaWrapperStructFor3)0;ret.inner = inner;return ret;}
[shader("intersection")]
void intersect_return()
{
ReportHit(0.5, 5u, NagaWrapperStructFor3Construct(0u));    return;
}

Container ConstructContainer(uint arg0) {
    Container ret = (Container)0;
    ret.inner = arg0;
    return ret;
}

[shader("intersection")]
void intersect_struct()
{
ReportHit(0.5, 5u, NagaWrapperStructFor9Construct(ConstructContainer(0u)));    return;
}
