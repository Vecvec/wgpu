use crate::back::hlsl::BackendResult;
use crate::{RayQueryIntersection, Statement, TypeInner};
use std::fmt::Write;

pub const MAP_HIT_NAME: &str = "map_hit";
pub const MAP_FRONT_FACE_NAME: &str = "map_front_face";

impl<W: Write> super::Writer<'_, W> {
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
    pub(super) fn write_committed_intersection_function(
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
            "        ret.instance_custom_index = rq.CommittedInstanceID();"
        )?;
        writeln!(
            self.out,
            "        ret.instance_id = rq.CommittedInstanceIndex();"
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
    pub(super) fn write_candidate_intersection_function(
        &mut self,
        module: &crate::Module,
    ) -> BackendResult {
        self.write_type(module, module.special_types.ray_intersection.unwrap())?;
        write!(self.out, " GetCandidateIntersection(")?;
        self.write_value_type(module, &TypeInner::RayQuery)?;
        writeln!(self.out, " rq) {{")?;
        write!(self.out, "    ")?;
        self.write_type(module, module.special_types.ray_intersection.unwrap())?;
        write!(self.out, " ret = (")?;
        self.write_type(module, module.special_types.ray_intersection.unwrap())?;
        writeln!(self.out, ")0;")?;
        writeln!(self.out, "    CANDIDATE_TYPE kind = rq.CandidateType();")?;
        writeln!(
            self.out,
            "    if (kind == CANDIDATE_NON_OPAQUE_TRIANGLE) {{"
        )?;
        writeln!(
            self.out,
            "        ret.kind = {};",
            RayQueryIntersection::Triangle as u32
        )?;
        writeln!(self.out, "        ret.t = rq.CandidateTriangleRayT();")?;
        writeln!(
            self.out,
            "        ret.barycentrics = rq.CandidateTriangleBarycentrics();"
        )?;
        writeln!(
            self.out,
            "        ret.front_face = rq.CandidateTriangleFrontFace();"
        )?;
        writeln!(self.out, "    }} else {{")?;
        writeln!(
            self.out,
            "        ret.kind = {};",
            RayQueryIntersection::Aabb as u32
        )?;
        writeln!(self.out, "    }}")?;

        writeln!(
            self.out,
            "    ret.instance_custom_index = rq.CandidateInstanceID();"
        )?;
        writeln!(
            self.out,
            "    ret.instance_id = rq.CandidateInstanceIndex();"
        )?;
        writeln!(
            self.out,
            "    ret.sbt_record_offset = rq.CandidateInstanceContributionToHitGroupIndex();"
        )?;
        writeln!(
            self.out,
            "    ret.geometry_index = rq.CandidateGeometryIndex();"
        )?;
        writeln!(
            self.out,
            "    ret.primitive_index = rq.CandidatePrimitiveIndex();"
        )?;
        writeln!(
            self.out,
            "    ret.object_to_world = rq.CandidateObjectToWorld4x3();"
        )?;
        writeln!(
            self.out,
            "    ret.world_to_object = rq.CandidateWorldToObject4x3();"
        )?;
        writeln!(self.out, "    return ret;")?;
        writeln!(self.out, "}}")?;
        writeln!(self.out)?;
        Ok(())
    }
    // see https://microsoft.github.io/DirectX-Specs/d3d/Raytracing.html#hitkind
    pub(super) fn write_map_hit(&mut self) -> BackendResult {
        if self.written_hit_map {
            return Ok(());
        }
        writeln!(
            self.out,
            "uint {MAP_HIT_NAME}(uint kind) {{\
    if (kind == 254 || kind == 255) {{\
        return {};
    }} else {{\
        return kind;
    }}\
}}\
",
            RayQueryIntersection::Triangle as u32
        )?;
        Ok(())
    }

    pub(super) fn write_map_front_face(&mut self) -> BackendResult {
        if self.written_front_face_map {
            return Ok(());
        }
        writeln!(
            self.out,
            "bool {MAP_FRONT_FACE_NAME}(uint kind) {{\
    if (kind == 254) {{\
        return true;
    }} else {{\
        return false;
    }}\
}}\
"
        )?;
        Ok(())
    }
    pub(super) fn write_wrapper_struct_from_block(&mut self, module: &crate::Module, block: &crate::Block) -> BackendResult {
        let mut blocks = Vec::new();
        blocks.push(block);
        // Prefer no recursion if possible - naga has issues with it and this might make it worse.
        // We don't need to have a particular order so we can just use a normal Vec.
        while let Some(block) = blocks.pop() {
            for statement in block.iter() {
                match *statement {
                    Statement::RayTracing { fun: crate::RayTracingFunction::ReportIntersection { intersection_ty, ..} } => {
                        let inner = &module.types[intersection_ty].inner;
                        match *inner {
                            TypeInner::Struct { .. } => {}
                            _ => {
                                // We need the constructor which also writes the struct.
                                self.write_wrapper_struct_constructor(module, intersection_ty)?
                            }
                        }
                        // There may only be one type for a given function
                        return Ok(());
                    }
                    Statement::Block(ref block) => blocks.push(block),
                    Statement::If { ref accept, ref reject , ..} => {
                        blocks.push(accept);
                        blocks.push(reject);
                    }
                    Statement::Call { function, ..} => {
                        let fun_block = &module.functions[function].body;
                        blocks.push(fun_block);
                    }
                    Statement::Loop { ref body, ref continuing,  .. } => {
                        blocks.push(body);
                        blocks.push(continuing);
                    },
                    Statement::Switch { ref cases, .. } => {
                        for case in cases.iter() {
                            blocks.push(&case.body)
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}
