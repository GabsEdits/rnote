use gtk4::glib;
use p2d::bounding_volume::Aabb;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rnote_compose::color;
use rnote_compose::helpers::AabbHelpers;
use rnote_compose::shapes::Rectangle;
use rnote_compose::shapes::ShapeBehaviour;
use rnote_compose::transform::Transform;
use rnote_compose::transform::TransformBehaviour;
use serde::{Deserialize, Serialize};
use std::ops::Range;

use super::strokebehaviour::GeneratedStrokeImages;
use super::{Stroke, StrokeBehaviour};
use crate::document::Format;
use crate::engine::import::{PdfImportPageSpacing, PdfImportPrefs};
use crate::usvg_export::{self, TreeExportExt};
use crate::{render, DrawBehaviour};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename = "vectorimage")]
pub struct VectorImage {
    #[serde(rename = "svg_data")]
    pub svg_data: String,
    #[serde(
        rename = "intrinsic_size",
        with = "rnote_compose::serialize::na_vector2_f64_dp3"
    )]
    pub intrinsic_size: na::Vector2<f64>,
    #[serde(rename = "rectangle")]
    pub rectangle: Rectangle,
}

impl Default for VectorImage {
    fn default() -> Self {
        Self {
            svg_data: String::default(),
            intrinsic_size: na::Vector2::zeros(),
            rectangle: Rectangle::default(),
        }
    }
}

impl StrokeBehaviour for VectorImage {
    fn gen_svg(&self) -> Result<render::Svg, anyhow::Error> {
        let svg_root = svg::node::element::SVG::new()
            .set("x", -self.rectangle.cuboid.half_extents[0])
            .set("y", -self.rectangle.cuboid.half_extents[1])
            .set("width", 2.0 * self.rectangle.cuboid.half_extents[0])
            .set("height", 2.0 * self.rectangle.cuboid.half_extents[1])
            .set(
                "viewBox",
                format!(
                    "{:.3} {:.3} {:.3} {:.3}",
                    0.0, 0.0, self.intrinsic_size[0], self.intrinsic_size[1]
                ),
            )
            .set("preserveAspectRatio", "none")
            .add(svg::node::Text::new(self.svg_data.clone()));

        let group = svg::node::element::Group::new()
            .set(
                "transform",
                self.rectangle.transform.to_svg_transform_attr_str(),
            )
            .add(svg_root);

        let svg_data = rnote_compose::utils::svg_node_to_string(&group)?;
        let svg = render::Svg {
            bounds: self.rectangle.bounds(),
            svg_data,
        };

        Ok(svg)
    }

    fn gen_images(
        &self,
        _viewport: Aabb,
        image_scale: f64,
    ) -> Result<GeneratedStrokeImages, anyhow::Error> {
        let bounds = self.bounds();

        // Always generate full stroke images for vectorimages, as they are too expensive to be repeatedly rendered
        Ok(GeneratedStrokeImages::Full(vec![
            render::Image::gen_with_piet(
                |piet_cx| self.draw(piet_cx, image_scale),
                bounds,
                image_scale,
            )?,
        ]))
    }
}

// Because we can't render svgs directly in piet, so we need to overwrite the gen_svgs() default implementation and call it in draw().
// There we use a svg renderer to generate pixel images. In this way we ensure to export an actual svg when calling gen_svgs(), but can also draw it onto piet.
impl DrawBehaviour for VectorImage {
    fn draw(&self, cx: &mut impl piet::RenderContext, image_scale: f64) -> anyhow::Result<()> {
        cx.save().map_err(|e| anyhow::anyhow!("{e:?}"))?;

        let image = render::Image::gen_image_from_svg(self.gen_svg()?, self.bounds(), image_scale)?;

        // image_scale does not have a meaning here, as the pixel image is already provided
        image.draw(cx, image_scale)?;

        cx.restore().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        Ok(())
    }
}

impl ShapeBehaviour for VectorImage {
    fn bounds(&self) -> Aabb {
        self.rectangle.bounds()
    }

    fn hitboxes(&self) -> Vec<Aabb> {
        vec![self.bounds()]
    }
}

impl TransformBehaviour for VectorImage {
    fn translate(&mut self, offset: na::Vector2<f64>) {
        self.rectangle.translate(offset);
    }

    fn rotate(&mut self, angle: f64, center: na::Point2<f64>) {
        self.rectangle.rotate(angle, center);
    }

    fn scale(&mut self, scale: na::Vector2<f64>) {
        self.rectangle.scale(scale);
    }
}

impl VectorImage {
    pub fn import_from_svg_data(
        svg_data: &str,
        pos: na::Vector2<f64>,
        size: Option<na::Vector2<f64>>,
    ) -> Result<Self, anyhow::Error> {
        let xml_options = usvg_export::ExportOptions {
            id_prefix: Some(rnote_compose::utils::svg_random_id_prefix()),
            n_decimal_places: 3,
            writer_opts: xmlwriter::Options {
                use_single_quote: false,
                indent: xmlwriter::Indent::None,
                attributes_indent: xmlwriter::Indent::None,
            },
        };

        let mut svg_tree = usvg::Tree::from_str(svg_data, &usvg::Options::default())?;
        svg_tree.convert_text_to_paths(&render::USVG_FONTDB);
        let svg_data = svg_tree.to_string(&xml_options);
        let intrinsic_size = na::vector![svg_tree.size.width(), svg_tree.size.height()];

        let rectangle = if let Some(size) = size {
            Rectangle {
                cuboid: p2d::shape::Cuboid::new(size * 0.5),
                transform: Transform::new_w_isometry(na::Isometry2::new(pos + size * 0.5, 0.0)),
            }
        } else {
            Rectangle {
                cuboid: p2d::shape::Cuboid::new(intrinsic_size * 0.5),
                transform: Transform::new_w_isometry(na::Isometry2::new(
                    pos + intrinsic_size * 0.5,
                    0.0,
                )),
            }
        };

        Ok(Self {
            svg_data,
            intrinsic_size,
            rectangle,
        })
    }

    pub fn import_from_pdf_bytes(
        to_be_read: &[u8],
        pdf_import_prefs: PdfImportPrefs,
        insert_pos: na::Vector2<f64>,
        page_range: Option<Range<u32>>,
        format: &Format,
    ) -> Result<Vec<Self>, anyhow::Error> {
        let doc = poppler::Document::from_bytes(&glib::Bytes::from(to_be_read), None)?;
        let page_range = page_range.unwrap_or(0..doc.n_pages() as u32);

        let page_width = format.width * (pdf_import_prefs.page_width_perc / 100.0);
        // calculate the page zoom based on the width of the first page.
        let page_zoom = if let Some(first_page) = doc.page(0) {
            page_width / first_page.size().0
        } else {
            return Ok(vec![]);
        };
        let x = insert_pos[0];
        let mut y = insert_pos[1];

        let svgs = page_range.filter_map(|page_i| {
            let page = doc.page(page_i as i32)?;
            let intrinsic_size = page.size();
            let width = intrinsic_size.0 * page_zoom;
            let height = intrinsic_size.1 * page_zoom;

            let res = move || -> anyhow::Result<String> {
                let svg_stream: Vec<u8> = vec![];

                let mut svg_surface =
                    cairo::SvgSurface::for_stream(intrinsic_size.0, intrinsic_size.1, svg_stream)
                        .map_err(|e| {
                        anyhow::anyhow!(
                            "create SvgSurface with dimensions ({}, {}) failed in vectorimage import_from_pdf_bytes with Err: {e:?}",
                            intrinsic_size.0,
                            intrinsic_size.1
                        )
                    })?;

                // Popplers page units are in points ( ^= 1 / 72 inch )
                svg_surface.set_document_unit(cairo::SvgUnit::Pt);

                {
                    let cx = cairo::Context::new(&svg_surface).map_err(|e| {
                        anyhow::anyhow!(
                            "new cairo::Context failed in vectorimage import_from_pdf_bytes() with Err: {e:?}"
                        )
                    })?;

                    // Set margin to white
                    cx.set_source_rgba(1.0, 1.0, 1.0, 1.0);
                    cx.paint()?;

                    // Render the poppler page
                    page.render_for_printing(&cx);

                    // Draw outline around page
                    cx.set_source_rgba(color::GNOME_REDS[4].as_rgba().0, color::GNOME_REDS[4].as_rgba().1, color::GNOME_REDS[4].as_rgba().2, 1.0);

                    let line_width = 1.0;
                    cx.set_line_width(line_width);
                    cx.rectangle(
                        line_width * 0.5,
                        line_width * 0.5,
                        intrinsic_size.0 - line_width,
                        intrinsic_size.1 - line_width,
                    );
                    cx.stroke()?;
                }

                let svg_content = String::from_utf8(
                    *svg_surface.finish_output_stream()
                        .map_err(|e| anyhow::anyhow!("{e:?}"))?
                        .downcast::<Vec<u8>>()
                        .map_err(|_e| anyhow::anyhow!("failed to downcast svg surface content in VectorImage import_from_pdf_bytes()"))?)?;


                Ok(svg_content)
            };

            let bounds = Aabb::new(na::point![x, y], na::point![x + width, y + height]);

            y += match pdf_import_prefs.page_spacing {
                PdfImportPageSpacing::Continuous => height + Stroke::IMPORT_OFFSET_DEFAULT[1] * 0.5,
                PdfImportPageSpacing::OnePerDocumentPage => format.height
            };

            match res() {
                Ok(svg_data) => Some(render::Svg {
                    svg_data,
                    bounds,
                }),
                Err(e) => {
                    log::error!("importing page {page_i} from pdf failed with Err: {e:?}");
                    None
                }
            }
        }).collect::<Vec<render::Svg>>();

        Ok(svgs
            .into_par_iter()
            .filter_map(|svg| {
                match Self::import_from_svg_data(
                    svg.svg_data.as_str(),
                    svg.bounds.mins.coords,
                    Some(svg.bounds.extents()),
                ) {
                    Ok(vectorimage) => Some(vectorimage),
                    Err(e) => {
                        log::error!("import_from_svg_data() failed failed in vectorimage import_from_pdf_bytes() with Err: {e:?}");
                        None
                    }
                }
            })
            .collect())
    }

    pub fn export_as_svg(&self) -> Result<String, anyhow::Error> {
        let export_bounds = self.bounds().translate(-self.bounds().mins.coords);

        let mut export_svg_data = self.gen_svg()?.svg_data;

        export_svg_data = rnote_compose::utils::wrap_svg_root(
            export_svg_data.as_str(),
            Some(export_bounds),
            Some(export_bounds),
            false,
        );

        Ok(export_svg_data)
    }
}
