use anyhow::Context;
use gtk4::{gdk, gio, glib, graphene, gsk, prelude::*, Snapshot};
use image::io::Reader;
use once_cell::sync::Lazy;
use p2d::bounding_volume::{Aabb, BoundingVolume};
use piet::RenderContext;
use rnote_compose::helpers::{AabbHelpers, Vector2Helpers};
use rnote_compose::shapes::{Rectangle, ShapeBehaviour};
use rnote_compose::transform::TransformBehaviour;
use serde::{Deserialize, Serialize};
use std::io::{self, Cursor};
use std::ops::Deref;
use svg::Node;

use crate::usvg_export::{self, TreeExportExt};
use crate::utils::GrapheneRectHelpers;
use crate::DrawBehaviour;

pub static USVG_FONTDB: Lazy<usvg_text_layout::fontdb::Database> = Lazy::new(|| {
    let mut db = usvg_text_layout::fontdb::Database::new();
    db.load_system_fonts();
    db
});

// Px unit (96 DPI ) to Point unit ( 72 DPI ) conversion factor
pub const PX_TO_POINT_CONV_FACTOR: f64 = 96.0 / 72.0;
// Point unit ( 72 DPI ) to Px unit (96 DPI ) conversion factor
pub const POINT_TO_PX_CONV_FACTOR: f64 = 72.0 / 96.0;
// the factor the rendering for the current viewport is extended. e.g.: 1.0 means the viewport is extended by its extents on all sides.
// Used when checking rendering for new zooms or a moved viewport.
// There is a trade off: a larger value will consume more ram, a smaller value will mean more stuttering on zooms and when moving the view
pub const VIEWPORT_EXTENTS_MARGIN_FACTOR: f64 = 0.4;

#[non_exhaustive]
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub enum ImageMemoryFormat {
    R8g8b8a8Premultiplied,
}

impl Default for ImageMemoryFormat {
    fn default() -> Self {
        Self::R8g8b8a8Premultiplied
    }
}

impl TryFrom<gdk::MemoryFormat> for ImageMemoryFormat {
    type Error = anyhow::Error;
    fn try_from(value: gdk::MemoryFormat) -> Result<Self, Self::Error> {
        match value {
            gdk::MemoryFormat::R8g8b8a8Premultiplied => Ok(Self::R8g8b8a8Premultiplied),
            _ => Err(anyhow::anyhow!(
                "ImageMemoryFormat try_from() gdk::MemoryFormat failed, unsupported MemoryFormat `{:?}`",
                value
            )),
        }
    }
}

/// From impl ImageMemoryFormat into gdk::MemoryFormat
impl From<ImageMemoryFormat> for gdk::MemoryFormat {
    fn from(value: ImageMemoryFormat) -> Self {
        match value {
            ImageMemoryFormat::R8g8b8a8Premultiplied => gdk::MemoryFormat::R8g8b8a8Premultiplied,
        }
    }
}

impl From<ImageMemoryFormat> for piet::ImageFormat {
    fn from(value: ImageMemoryFormat) -> Self {
        match value {
            ImageMemoryFormat::R8g8b8a8Premultiplied => piet::ImageFormat::RgbaPremul,
        }
    }
}

/// A pixel image
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename = "image")]
pub struct Image {
    /// The image data. is (de) serialized in base64 encoding
    #[serde(rename = "data", with = "rnote_compose::serialize::vecu8_base64")]
    pub data: Vec<u8>,
    /// the target rect in the coordinate space of the doc
    #[serde(rename = "rectangle")]
    pub rect: Rectangle,
    /// width of the data
    #[serde(rename = "pixel_width")]
    pub pixel_width: u32,
    /// height of the data
    #[serde(rename = "pixel_height")]
    pub pixel_height: u32,
    /// the memory format
    #[serde(rename = "memory_format")]
    pub memory_format: ImageMemoryFormat,
}

impl Default for Image {
    fn default() -> Self {
        Self {
            data: vec![],
            rect: Rectangle::default(),
            pixel_width: 0,
            pixel_height: 0,
            memory_format: ImageMemoryFormat::default(),
        }
    }
}

impl From<image::DynamicImage> for Image {
    fn from(dynamic_image: image::DynamicImage) -> Self {
        let pixel_width = dynamic_image.width();
        let pixel_height = dynamic_image.height();
        let memory_format = ImageMemoryFormat::R8g8b8a8Premultiplied;
        let data = dynamic_image.into_rgba8().to_vec();

        let bounds = Aabb::new(
            na::point![0.0, 0.0],
            na::point![f64::from(pixel_width), f64::from(pixel_height)],
        );

        Self {
            data,
            rect: Rectangle::from_p2d_aabb(bounds),
            pixel_width,
            pixel_height,
            memory_format,
        }
    }
}

impl DrawBehaviour for Image {
    /// Expects image to be in rgba8-premultiplied format, else drawing will fail.
    /// image_scale has no meaning here, as the image pixels are already provided
    fn draw(&self, cx: &mut impl piet::RenderContext, _image_scale: f64) -> anyhow::Result<()> {
        cx.save().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let piet_image_format = piet::ImageFormat::try_from(self.memory_format)?;

        let piet_image = cx
            .make_image(
                self.pixel_width as usize,
                self.pixel_height as usize,
                &self.data,
                piet_image_format,
            )
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        cx.transform(self.rect.transform.to_kurbo());

        cx.draw_image(
            &piet_image,
            self.rect.cuboid.local_aabb().to_kurbo_rect(),
            piet::InterpolationMode::Bilinear,
        );
        cx.restore().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        Ok(())
    }
}

impl TransformBehaviour for Image {
    fn translate(&mut self, offset: na::Vector2<f64>) {
        self.rect.translate(offset)
    }

    fn rotate(&mut self, angle: f64, center: na::Point2<f64>) {
        self.rect.rotate(angle, center)
    }

    fn scale(&mut self, scale: na::Vector2<f64>) {
        self.rect.scale(scale)
    }
}

impl Image {
    pub fn assert_valid(&self) -> anyhow::Result<()> {
        self.rect.bounds().assert_valid()?;

        if self.pixel_width == 0
            || self.pixel_height == 0
            || self.data.len() as u32 != 4 * self.pixel_width * self.pixel_height
        {
            Err(anyhow::anyhow!(
                "assert_image() failed, invalid size or data"
            ))
        } else {
            Ok(())
        }
    }

    pub fn try_from_encoded_bytes(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        let reader = Reader::new(io::Cursor::new(bytes)).with_guessed_format()?;
        Ok(Image::from(reader.decode()?))
    }

    pub fn to_imgbuf(self) -> Result<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>, anyhow::Error> {
        self.assert_valid()?;

        match self.memory_format {
            ImageMemoryFormat::R8g8b8a8Premultiplied => {
                image::RgbaImage::from_vec(self.pixel_width, self.pixel_height, self.data)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                    "RgbaImage::from_vec() failed in Image to_imgbuf() for image with Format {:?}",
                    self.memory_format
                )
                    })
            }
        }
    }

    pub fn into_encoded_bytes(
        self,
        format: image::ImageOutputFormat,
    ) -> Result<Vec<u8>, anyhow::Error> {
        self.assert_valid()?;
        let mut bytes_buf: Cursor<Vec<u8>> = Cursor::new(Vec::new());

        let dynamic_image = image::DynamicImage::ImageRgba8(
            self.to_imgbuf()
                .context("image.to_imgbuf() failed in image_to_bytes()")?,
        );
        dynamic_image
            .write_to(&mut bytes_buf, format)
            .context("dynamic_image.write_to() failed in image_to_bytes()")?;

        Ok(bytes_buf.into_inner())
    }

    pub fn to_memtexture(&self) -> Result<gdk::MemoryTexture, anyhow::Error> {
        self.assert_valid()?;

        let bytes = self.data.deref();

        Ok(gdk::MemoryTexture::new(
            self.pixel_width as i32,
            self.pixel_height as i32,
            self.memory_format.into(),
            &glib::Bytes::from(bytes),
            (self.pixel_width * 4) as usize,
        ))
    }

    pub fn to_rendernode(
        &self,
        rect_override: Option<Rectangle>,
    ) -> Result<gsk::RenderNode, anyhow::Error> {
        self.assert_valid()?;

        let memtexture = self.to_memtexture()?;

        let rect = rect_override.unwrap_or(self.rect);

        let texture_node = gsk::TextureNode::new(
            &memtexture,
            &graphene::Rect::from_p2d_aabb(rect.cuboid.local_aabb()),
        )
        .upcast();

        let transform_node = gsk::TransformNode::new(
            &texture_node,
            &crate::utils::transform_to_gsk(&rect.transform),
        )
        .upcast();
        Ok(transform_node)
    }

    pub fn images_to_rendernodes<'a>(
        images: impl IntoIterator<Item = &'a Self>,
    ) -> Result<Vec<gsk::RenderNode>, anyhow::Error> {
        let mut rendernodes = vec![];

        for image in images {
            rendernodes.push(image.to_rendernode(None)?)
        }

        Ok(rendernodes)
    }

    pub fn append_images_to_rendernode(
        images: &[Self],
        rendernode: Option<&gsk::RenderNode>,
    ) -> Result<Option<gsk::RenderNode>, anyhow::Error> {
        let snapshot = Snapshot::new();

        if let Some(rendernode) = rendernode {
            snapshot.append_node(rendernode);
        }

        for image in images {
            snapshot.append_node(
                &image
                    .to_rendernode(None)
                    .context("image_to_rendernode() failed in append_images_to_rendernode()")?,
            );
        }

        Ok(snapshot.to_node())
    }

    /// create an image from an svg (using librsvg )
    pub fn gen_image_from_svg(
        svg: Svg,
        mut bounds: Aabb,
        image_scale: f64,
    ) -> Result<Self, anyhow::Error> {
        let svg_data = rnote_compose::utils::wrap_svg_root(
            svg.svg_data.as_str(),
            Some(bounds),
            Some(bounds),
            false,
        );

        bounds.ensure_positive();
        bounds = bounds.ceil().loosened(1.0);
        bounds.assert_valid()?;

        let width_scaled = ((bounds.extents()[0]) * image_scale).round() as u32;
        let height_scaled = ((bounds.extents()[1]) * image_scale).round() as u32;

        let mut surface = cairo::ImageSurface::create(
                cairo::Format::ARgb32,
                width_scaled as i32,
                height_scaled as i32,
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "create ImageSurface with dimensions ({width_scaled}, {height_scaled}) failed in gen_image_from_svg(), Err: {e:?}"
                )
            })?;

        // Context in new scope, else accessing the surface data fails with a borrow error
        {
            let cx = cairo::Context::new(&surface)
                .context("new cairo::Context failed in gen_image_from_svg()")?;
            cx.scale(image_scale, image_scale);
            cx.translate(-bounds.mins[0], -bounds.mins[1]);

            let stream =
                gio::MemoryInputStream::from_bytes(&glib::Bytes::from(svg_data.as_bytes()));

            let handle = rsvg::Loader::new()
                .read_stream::<gio::MemoryInputStream, gio::File, gio::Cancellable>(
                    &stream, None, None,
                )
                .context("read stream to librsvg Loader failed in gen_image_from_svg()")?;
            let renderer = rsvg::CairoRenderer::new(&handle);
            renderer
                .render_document(
                    &cx,
                    &cairo::Rectangle::new(
                        bounds.mins[0],
                        bounds.mins[1],
                        bounds.extents()[0],
                        bounds.extents()[1],
                    ),
                )
                .map_err(|e| {
                    anyhow::Error::msg(format!(
                        "librsvg render_document() failed in gen_image_from_svg() with Err: {e:?}"
                    ))
                })?;
        }
        // Surface needs to be flushed before accessing its data
        surface.flush();

        let data = surface
            .data()
            .map_err(|e| {
                anyhow::Error::msg(format!(
                    "accessing imagesurface data failed in gen_image_from_svg() with Err: {e:?}"
                ))
            })?
            .to_vec();

        Ok(Self {
            data: convert_image_bgra_to_rgba(width_scaled, height_scaled, data),
            rect: Rectangle::from_p2d_aabb(bounds),
            pixel_width: width_scaled,
            pixel_height: height_scaled,
            // cairo renders to bgra8-premultiplied, but we convert it to rgba8-premultiplied
            memory_format: ImageMemoryFormat::R8g8b8a8Premultiplied,
        })
    }

    /// Renders an image with a function that draws onto a piet CairoRenderContext
    pub fn gen_with_piet<F>(
        draw_func: F,
        mut bounds: Aabb,
        image_scale: f64,
    ) -> anyhow::Result<Self>
    where
        F: FnOnce(&mut piet_cairo::CairoRenderContext) -> anyhow::Result<()>,
    {
        bounds.ensure_positive();
        bounds = bounds.ceil().loosened(1.0);
        bounds.assert_valid()?;

        let width_scaled = ((bounds.extents()[0]) * image_scale).round() as u32;
        let height_scaled = ((bounds.extents()[1]) * image_scale).round() as u32;

        let mut image_surface = cairo::ImageSurface::create(
            cairo::Format::ARgb32,
            width_scaled as i32,
            height_scaled as i32,
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "create ImageSurface with dimensions ({}, {}) failed in Image gen_with_piet(), {}",
                width_scaled,
                height_scaled,
                e
            )
        })?;

        {
            let cairo_cx = cairo::Context::new(&image_surface)?;
            let mut piet_cx = piet_cairo::CairoRenderContext::new(&cairo_cx);

            piet_cx.transform(kurbo::Affine::scale(image_scale));
            piet_cx.transform(kurbo::Affine::translate(-bounds.mins.coords.to_kurbo_vec()));

            // Apply the draw function
            draw_func(&mut piet_cx)?;

            piet_cx.finish().map_err(|e| {
                anyhow::anyhow!("piet_cx.finish() failed in image.gen_with_piet() with Err: {e:?}")
            })?;
        }
        // Surface needs to be flushed before accessing its data
        image_surface.flush();

        let data = image_surface
                .data()
                .map_err(|e| {
                    anyhow::Error::msg(format!(
                "accessing imagesurface data failed in strokebehaviour image.gen_with_piet() with Err: {e:?}"
            ))
                })?
                .to_vec();

        Ok(Image {
            data: convert_image_bgra_to_rgba(width_scaled, height_scaled, data),
            rect: Rectangle::from_p2d_aabb(bounds),
            pixel_width: width_scaled,
            pixel_height: height_scaled,
            // cairo renders to bgra8-premultiplied, but we convert it to rgba8-premultiplied
            memory_format: ImageMemoryFormat::R8g8b8a8Premultiplied,
        })
    }
}

/// A Svg Image
#[derive(Debug, Clone)]
pub struct Svg {
    /// the svg data as String
    pub svg_data: String,
    /// the bounds of the svg
    pub bounds: Aabb,
}

impl Svg {
    pub fn merge<T>(&mut self, other: T)
    where
        T: IntoIterator<Item = Self>,
    {
        for svg in other {
            self.svg_data += format!("\n{}", svg.svg_data).as_str();
            self.bounds.merge(&svg.bounds);
        }
    }

    pub fn wrap_svg_root(
        &mut self,
        bounds: Option<Aabb>,
        viewbox: Option<Aabb>,
        preserve_aspectratio: bool,
    ) {
        self.svg_data = rnote_compose::utils::wrap_svg_root(
            self.svg_data.as_str(),
            bounds,
            viewbox,
            preserve_aspectratio,
        );
        if let Some(bounds) = bounds {
            self.bounds = bounds
        }
    }

    /// Generates an svg with piet, using the piet_cairo backend and a SvgSurface.
    /// This might be preferable to the piet_svg backend, because especially text alignment and sizes can be different with it.
    pub fn gen_with_piet_cairo_backend<F>(draw_func: F, mut bounds: Aabb) -> anyhow::Result<Self>
    where
        F: FnOnce(&mut piet_cairo::CairoRenderContext) -> anyhow::Result<()>,
    {
        bounds.ensure_positive();
        bounds.assert_valid()?;

        let width = bounds.extents()[0];
        let height = bounds.extents()[1];

        let svg_stream: Vec<u8> = vec![];

        let mut svg_surface = cairo::SvgSurface::for_stream(
            width, height, svg_stream
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "create SvgSurface with dimensions ({}, {}) failed in Svg gen_with_piet_cairo_backend(), {}",
                width,height,
                e
            )
        })?;

        svg_surface.set_document_unit(cairo::SvgUnit::Px);

        {
            let cairo_cx = cairo::Context::new(&svg_surface)?;
            let mut piet_cx = piet_cairo::CairoRenderContext::new(&cairo_cx);

            // Cairo only draws elements with positive coordinates, so we need to transform them here
            piet_cx.transform(kurbo::Affine::translate(-bounds.mins.coords.to_kurbo_vec()));

            // Apply the draw function
            draw_func(&mut piet_cx)?;

            piet_cx.finish().map_err(|e| {
                anyhow::anyhow!(
                    "piet_cx.finish() failed in Svg gen_with_piet_cairo_backend() with Err: {e:?}"
                )
            })?;
        }

        let file_content = svg_surface
            .finish_output_stream()
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        let svg_data = rnote_compose::utils::remove_xml_header(
            String::from_utf8(*file_content.downcast::<Vec<u8>>().map_err(|_e| {
                anyhow::anyhow!(
                    "failed to downcast svg surface content in Svg gen_with_piet_cairo_backend()"
                )
            })?)?
            .as_str(),
        );

        let mut group = svg::node::element::Group::new().add(svg::node::Text::new(svg_data));

        group.assign(
            "transform",
            format!("translate({} {})", bounds.mins[0], bounds.mins[1]),
        );

        Ok(Self {
            svg_data: rnote_compose::utils::svg_node_to_string(&group)?,
            bounds,
        })
    }

    pub fn draw_svgs_to_cairo_context(svgs: &[Self], cx: &cairo::Context) -> anyhow::Result<()> {
        for svg in svgs {
            let svg_data = rnote_compose::utils::wrap_svg_root(
                svg.svg_data.as_str(),
                Some(svg.bounds),
                Some(svg.bounds),
                false,
            );

            let stream =
                gio::MemoryInputStream::from_bytes(&glib::Bytes::from(svg_data.as_bytes()));

            let handle = rsvg::Loader::new()
                .read_stream::<gio::MemoryInputStream, gio::File, gio::Cancellable>(
                    &stream, None, None,
                )
                .context("read stream to librsvg Loader failed")?;

            let renderer = rsvg::CairoRenderer::new(&handle);
            renderer
                .render_document(
                    cx,
                    &cairo::Rectangle::new(
                        svg.bounds.mins[0],
                        svg.bounds.mins[1],
                        svg.bounds.extents()[0],
                        svg.bounds.extents()[1],
                    ),
                )
                .map_err(|e| {
                    anyhow::Error::msg(format!(
                    "librsvg render_document() failed in draw_svgs_to_cairo_context() with Err: {e:?}"
                ))
                })?;
        }
        Ok(())
    }

    /// Simplifies the svg by passing it through usvg
    pub fn simplify(&mut self) -> anyhow::Result<()> {
        let xml_options = usvg_export::ExportOptions {
            id_prefix: Some(rnote_compose::utils::svg_random_id_prefix()),
            n_decimal_places: 3,
            writer_opts: xmlwriter::Options {
                use_single_quote: false,
                indent: xmlwriter::Indent::None,
                attributes_indent: xmlwriter::Indent::None,
            },
        };
        let svg_data = rnote_compose::utils::wrap_svg_root(
            &rnote_compose::utils::remove_xml_header(&self.svg_data),
            None,
            None,
            false,
        );
        let mut usvg_tree = usvg::Tree::from_str(&svg_data, &usvg::Options::default())?;
        usvg_tree.convert_text_to_paths(&USVG_FONTDB);
        self.svg_data = rnote_compose::utils::remove_xml_header(&usvg_tree.to_string(&xml_options));

        Ok(())
    }

    #[allow(unused)]
    fn render_to_caironode(&self) -> Result<gsk::CairoNode, anyhow::Error> {
        if self.bounds.extents()[0] < 0.0 || self.bounds.extents()[1] < 0.0 {
            return Err(anyhow::anyhow!(
                "render_to_caironode() failed, bounds width/ height is < 0.0"
            ));
        }

        let new_caironode = gsk::CairoNode::new(&graphene::Rect::from_p2d_aabb(self.bounds));
        let cx = new_caironode.draw_context();

        Svg::draw_svgs_to_cairo_context(&[self.to_owned()], &cx)?;

        Ok(new_caironode)
    }
}

fn convert_image_bgra_to_rgba(_width: u32, _height: u32, mut bytes: Vec<u8>) -> Vec<u8> {
    for src in bytes.chunks_exact_mut(4) {
        let (blue, green, red, alpha) = (src[0], src[1], src[2], src[3]);
        src[0] = red;
        src[1] = green;
        src[2] = blue;
        src[3] = alpha;
    }
    bytes
}
