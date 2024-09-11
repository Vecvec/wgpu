use crate::back::hlsl::BackendResult;
use crate::TypeInner;
use std::fmt::Write;

impl<'a, W: Write> super::Writer<'a, W> {
    // constructs hlsl RayDesc from wgsl RayDesc
    pub(super) fn write_ray_desc_from_ray_desc_constructor_function(
        &mut self,
        module: &crate::Module,
    ) -> BackendResult {
        write!(self.out, "RayDesc RayDescFromRayDesc_(")?;
        self.write_type(module, module.special_types.ray_desc.unwrap())?;
        writeln!(self.out, " arg0) {{")?;
        writeln!(self.out, "    RayDesc ret = (RayDesc)0;")?;
        writeln!(self.out, "    ret.Origin = arg0.origin;")?;
        writeln!(self.out, "    ret.TMin = arg0.tmin;")?;
        writeln!(self.out, "    ret.Direction = arg0.dir;")?;
        writeln!(self.out, "    ret.TMax = arg0.tmax;")?;
        writeln!(self.out, "    return ret;")?;
        writeln!(self.out, "}}")?;
        writeln!(self.out)?;
        Ok(())
    }
    pub(super) fn write_commited_intersection_function(
        &mut self,
        module: &crate::Module,
    ) -> BackendResult {
        self.write_type(module, module.special_types.ray_intersection.unwrap())?;
        write!(self.out, " GetCommittedIntersection(")?;
        self.write_value_type(module, &TypeInner::RayQuery)?;
        writeln!(self.out, " rq) {{")?;
        write!(self.out, "    ")?;
        self.write_type(module, module.special_types.ray_intersection.unwrap())?;
        write!(self.out, " ret = (")?;
        self.write_type(module, module.special_types.ray_intersection.unwrap())?;
        writeln!(self.out, ")0;")?;
        writeln!(self.out, "    ret.kind = rq.CommittedStatus();")?;
        writeln!(
            self.out,
            "    if( rq.CommittedStatus() == COMMITTED_NOTHING) {{}} else {{"
        )?;
        writeln!(self.out, "        ret.t = rq.CommittedRayT();")?;
        writeln!(
            self.out,
            "        ret.instance_custom_index = rq.CommittedInstanceIndex();"
        )?;
        writeln!(
            self.out,
            "        ret.instance_id = rq.CommittedInstanceID();"
        )?;
        writeln!(
            self.out,
            "        ret.sbt_record_offset = rq.CommittedInstanceContributionToHitGroupIndex();"
        )?;
        writeln!(
            self.out,
            "        ret.geometry_index = rq.CommittedGeometryIndex();"
        )?;
        writeln!(
            self.out,
            "        ret.primitive_index = rq.CommittedPrimitiveIndex();"
        )?;
        writeln!(
            self.out,
            "        if( rq.CommittedStatus() == COMMITTED_TRIANGLE_HIT ) {{"
        )?;
        writeln!(
            self.out,
            "            ret.barycentrics = rq.CommittedTriangleBarycentrics();"
        )?;
        writeln!(
            self.out,
            "            ret.front_face = rq.CommittedTriangleFrontFace();"
        )?;
        writeln!(self.out, "        }}")?;
        writeln!(
            self.out,
            "        ret.object_to_world = rq.CommittedObjectToWorld4x3();"
        )?;
        writeln!(
            self.out,
            "        ret.world_to_object = rq.CommittedWorldToObject4x3();"
        )?;
        writeln!(self.out, "    }}")?;
        writeln!(self.out, "    return ret;")?;
        writeln!(self.out, "}}")?;
        writeln!(self.out)?;
        Ok(())
    }
}
