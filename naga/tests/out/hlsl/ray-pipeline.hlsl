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

struct TriRayIntersection {
    int _pad0_0;
    int _pad0_1;
    int _pad0_2;
    int _pad0_3;
    int _pad0_4;
    int _pad0_5;
    int _pad0_6;
    float2 barycentrics;
    int _end_pad_0;
    int _end_pad_1;
    int _end_pad_2;
    int _end_pad_3;
    int _end_pad_4;
    int _end_pad_5;
    int _end_pad_6;
    int _end_pad_7;
    int _end_pad_8;
    int _end_pad_9;
    int _end_pad_10;
    int _end_pad_11;
    int _end_pad_12;
    int _end_pad_13;
    int _end_pad_14;
    int _end_pad_15;
    int _end_pad_16;
    int _end_pad_17;
    int _end_pad_18;
    int _end_pad_19;
    int _end_pad_20;
    int _end_pad_21;
    int _end_pad_22;
    int _end_pad_23;
    int _end_pad_24;
    int _end_pad_25;
    int _end_pad_26;
    int _end_pad_27;
    int _end_pad_28;
    int _end_pad_29;
    int _end_pad_30;
    int _end_pad_31;
    int _end_pad_32;
    int _end_pad_33;
    int _end_pad_34;
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

TraceRay(acc_struct, ZeroValueRayDesc_().flags, ZeroValueRayDesc_().cull_mask, 0, 0, 0, { ZeroValueRayDesc_().origin, ZeroValueRayDesc_().tmin, ZeroValueRayDesc_().dir, ZeroValueRayDesc_().tmax}, colour_5);    return;
}

[shader("raygeneration")]
void ray_gen()
{
    float4 colour = ZeroValuefloat4();

TraceRay(acc_struct, ZeroValueRayDesc_().flags, ZeroValueRayDesc_().cull_mask, 0, 0, 0, { ZeroValueRayDesc_().origin, ZeroValueRayDesc_().tmin, ZeroValueRayDesc_().dir, ZeroValueRayDesc_().tmax}, colour);    trace();
    return;
}

[shader("anyhit")]
void discard_any_hit(inout colour_1, in intersection)
{
    colour_1 = ZeroValuefloat4();
    discard;
}

[shader("anyhit")]
void any_hit(inout colour_2, in intersection_1)
{
float t = RayTCurrent();
    colour_2 = ZeroValuefloat4();
    return;
}

[shader("closesthit")]
void closest_hit(inout colour_3, in intersection_2)
{
float t_1 = RayTCurrent();
    colour_3 = (1.0).xxxx;
    return;
}

[shader("miss")]
void miss(inout colour_4)
{
    colour_4 = ZeroValuefloat4();
    return;
}

[shader("intersection")]
void intersect_return(inout naga_payload, in naga_intersection)
{
ReportHit(0.5, 5u, 0u);    return;
}
