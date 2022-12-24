use crate::*;

use crate::bindings::*;

use ndarray::{Array, ArrayView, ArrayViewMut, IxDyn};

type Result<T> = std::result::Result<T, OrtStatusPtr>;

pub enum ExecutionProviders {
    Cpu,
}

#[derive(Debug)]
pub struct Api {
    api: &'static OrtApi,
}

#[derive(Debug)]
pub struct KernelContext<'s> {
    context: &'s mut OrtKernelContext,
    api: &'s Api,
}

#[derive(Debug)]
pub struct KernelInfo<'s> {
    api: &'s OrtApi,
    info: &'s OrtKernelInfo,
}

// From context
#[derive(Debug)]
pub struct Value<'s> {
    value: &'s mut OrtValue,
    api: &'s Api,
}

#[derive(Debug)]
pub struct OutputValue<'s> {
    api: Api,
    context: &'s mut OrtKernelContext,
    index: u64,
}

// From Value
#[derive(Debug)]
pub struct TensorTypeAndShapeInfo<'s> {
    api: &'s Api,
    info: &'s mut OrtTensorTypeAndShapeInfo,
}

pub struct CustomOpDomain<'s> {
    api: &'s OrtApi,
    custom_op_domain: &'s mut OrtCustomOpDomain,
}

pub enum ElementType {
    F32,
    F64,
    I32,
    I64,
    String,
}

pub struct SessionOptions<'s> {
    api: &'static OrtApi,
    session_options: &'s mut OrtSessionOptions,
}

impl std::ops::Deref for Api {
    type Target = OrtApi;

    fn deref(&self) -> &Self::Target {
        self.api
    }
}

impl Api {
    pub fn from_raw(api: &'static OrtApi) -> Self {
        Self { api }
    }

    fn create_error_status(&self, code: u32, msg: &str) -> *mut OrtStatus {
        let c_char_ptr = str_to_c_char_ptr(msg);
        unsafe { self.CreateStatus.unwrap()(code, c_char_ptr) }
    }
}

impl<'s> KernelContext<'s> {
    pub fn from_raw(api: &'s Api, context: *mut OrtKernelContext) -> Self {
        unsafe {
            Self {
                api,
                context: &mut *context,
            }
        }
    }

    pub fn get_input_value(&self, index: u64) -> Result<Value<'s>> {
        let mut value: *const OrtValue = std::ptr::null();
        status_to_result(unsafe {
            self.api.KernelContext_GetInput.unwrap()(self.context, index, &mut value)
        })?;
        if value.is_null() {
            status_to_result(
                self.api
                    .create_error_status(OrtErrorCode_ORT_FAIL, "Failed to get input"),
            )?;
        }
        let value = unsafe { &mut *(value as *mut OrtValue) };
        Ok(Value {
            value,
            api: self.api,
        })
    }

    // pub fn get_input<T>(&self, index: u64) -> Result<ArrayView<T, IxDyn>> {
    //     let mut value = self.get_input_value(index)?;
    //     value.get_tensor_data::<T>()
    // }

    pub unsafe fn get_safe_output(self, index: u64) -> OutputValue<'s> {
        OutputValue::<'s> {
            api: Api::from_raw(self.api.api),
            context: self.context,
            index,
        }
    }

    pub fn get_output_count(&self) -> Result<u64> {
        let mut val = 0;
        status_to_result(unsafe {
            self.api.KernelContext_GetOutputCount.unwrap()(self.context, &mut val)
        })?;
        Ok(val)
    }

    #[allow(unused)]
    pub fn get_input_count(&self) -> Result<u64> {
        let mut val = 0;
        status_to_result(unsafe {
            self.api.KernelContext_GetInputCount.unwrap()(self.context, &mut val)
        })?;
        Ok(val)
    }
}

impl<'s> KernelInfo<'s> {
    pub fn from_ort<'inner>(
        api: &'inner OrtApi,
        info: &'inner OrtKernelInfo,
    ) -> KernelInfo<'inner> {
        KernelInfo { api, info }
    }

    pub fn get_attribute_string(&self, name: &str) -> Result<String> {
        let name = CString::new(name).unwrap();
        // Get size first
        let mut size = {
            let mut size = 0;
            // let buf: *mut _ = std::ptr::null_mut();
            unsafe {
                status_to_result_dbg(
                    self.api.KernelInfoGetAttribute_string.unwrap()(
                        self.info,
                        name.as_ptr(),
                        std::ptr::null_mut(),
                        &mut size,
                    ),
                    self.api,
                )?;
                size
            }
        };

        let mut buf = vec![0u8; size as _];
        unsafe {
            status_to_result(self.api.KernelInfoGetAttribute_string.unwrap()(
                self.info,
                name.as_ptr(),
                buf.as_mut_ptr() as *mut i8,
                &mut size,
            ))
            .unwrap()
        };
        Ok(CString::from_vec_with_nul(buf)
            .unwrap()
            .into_string()
            .unwrap())
    }

    // Not implemented for string?
    #[allow(unused)]
    fn get_attribute_array<T>(&self, name: &str) -> Result<&[T]> {
        unimplemented!()
    }
}

// Should be enum
#[allow(unused)]
type Type = ONNXType;

impl<'s> Value<'s> {
    // The API seems to force us to copy the data, hence the owned type...
    pub fn get_tensor_data_str(self) -> Result<Array<String, IxDyn>> {
        // GetTensorMutableData is not supposed to be used for
        // strings, according to the api docs.

        // number of strings
        let item_count = self
            .get_tensor_type_and_shape()?
            .get_tensor_shape_element_count()?;
        // total number of bytes of all concatenated strings (no trailing nulls!)
        let non_null_bytes = {
            let mut non_null_bytes: u64 = 0;
            let fun_ptr = self.api.GetStringTensorDataLength.unwrap();
            unsafe { fun_ptr(self.value, &mut non_null_bytes) };
            non_null_bytes
        };

        // Read all strings concatenated into one string. Seems odd,
        // but that is what the api offers...
        let strings: Vec<_> = {
            let fun_ptr = self.api.GetStringTensorContent.unwrap();
            let mut buf = vec![0u8; non_null_bytes as usize];
            let mut offsets = vec![0usize; item_count as usize];
            unsafe {
                fun_ptr(
                    self.value,
                    buf.as_mut_ptr() as *mut _,
                    non_null_bytes,
                    offsets.as_mut_ptr() as *mut _,
                    offsets.len() as u64,
                );
            }

            // Compute windows with the start and end of each
            // substring and then scan the buffer.
            let very_end = [non_null_bytes as usize];
            let starts = offsets.iter();
            let ends = offsets.iter().chain(very_end.iter()).skip(1);
            let windows = starts.zip(ends);
            windows
                .scan(buf.as_slice(), |buf: &mut &[u8], (start, end)| {
                    let (this, rest) = buf.split_at(end - start);
                    *buf = rest;
                    // The following allocation could be avoided
                    Some(String::from_utf8(this.to_vec()).unwrap())
                })
                .collect()
        };
        // Figure out the correct shape
        let dims: Vec<_> = {
            let info = self.get_tensor_type_and_shape()?;
            info.get_dimensions()?
                .into_iter()
                .map(|el| el as usize)
                .collect()
        };
        Ok(Array::from(strings)
            .into_shape(dims)
            .expect("Shape information was incorrect."))
    }

    pub fn get_tensor_data<T>(self) -> Result<ArrayView<'s, T, IxDyn>> {
        // Get the data
        let data = {
            let element_count = self
                .get_tensor_type_and_shape()?
                .get_tensor_shape_element_count()?;

            let mut ptr: *mut _ = std::ptr::null_mut();
            unsafe {
                self.api.GetTensorMutableData.unwrap()(self.value, &mut ptr);
                std::slice::from_raw_parts(ptr as *mut T, element_count as usize)
            }
        };
        // Figure out the correct shape
        let dims: Vec<_> = {
            let info = self.get_tensor_type_and_shape()?;
            info.get_dimensions()?
                .into_iter()
                .map(|el| el as usize)
                .collect()
        };
        Ok(ArrayView::from(data)
            .into_shape(dims)
            .expect("Shape information was incorrect."))
    }

    pub fn get_tensor_type_and_shape(&self) -> Result<TensorTypeAndShapeInfo<'s>> {
        let mut info: *mut OrtTensorTypeAndShapeInfo = std::ptr::null_mut();
        unsafe {
            self.api.GetTensorTypeAndShape.unwrap()(self.value, &mut info);
            Ok(TensorTypeAndShapeInfo {
                api: self.api,
                info: &mut *info,
            })
        }
    }

    #[allow(unused)]
    pub fn get_value_type(&self) -> Result<Type> {
        unimplemented!()
    }

    // /// Not clear what that is...
    // fn get_type_info(&self) -> Result<TypeInfo> {
    // 	unimplemented!()
    // }
}

impl<'s> TensorTypeAndShapeInfo<'s> {
    pub fn get_dimensions(&self) -> Result<Vec<i64>> {
        let mut n_dim = 0;
        unsafe { self.api.GetDimensionsCount.unwrap()(self.info, &mut n_dim) };
        let mut out = Vec::with_capacity(n_dim as usize);
        unsafe {
            self.api.GetDimensions.unwrap()(self.info, out.as_mut_ptr(), n_dim);
            out.set_len(n_dim as usize);
        }
        Ok(out)
    }

    fn get_tensor_shape_element_count(&self) -> Result<u64> {
        let mut element_count = 0;
        status_to_result(unsafe {
            self.api.GetTensorShapeElementCount.unwrap()(self.info, &mut element_count)
        })?;
        Ok(element_count)
    }

    #[allow(unused)]
    fn get_tensor_element_type(&self) -> Result<ONNXTensorElementDataType> {
        unimplemented!()
    }
    //...
}

impl<'s> Drop for TensorTypeAndShapeInfo<'s> {
    fn drop(&mut self) {
        unsafe { self.api.ReleaseTensorTypeAndShapeInfo.unwrap()(&mut *self.info) }
    }
}

impl<'s> Drop for Value<'s> {
    fn drop(&mut self) {
        // This is unused in the official example and crashes if used...
    }
}

impl<'s> CustomOpDomain<'s> {
    pub fn add_op_to_domain(&mut self, op: &'static OrtCustomOp) -> Result<()> {
        let fun_ptr = self.api.CustomOpDomain_Add.unwrap();
        status_to_result(unsafe { fun_ptr(self.custom_op_domain, op) })?;
        Ok(())
    }
}

impl<'s> OutputValue<'s> {
    pub fn get_tensor_data_mut<T>(self, shape: &[usize]) -> Result<ArrayViewMut<T, IxDyn>> {
        let value = unsafe {
            let mut value: *mut OrtValue = std::ptr::null_mut();
            let shape: Vec<_> = shape.iter().map(|el| *el as i64).collect();
            status_to_result(self.api.KernelContext_GetOutput.unwrap()(
                self.context,
                self.index,
                shape.as_ptr(),
                shape.len() as u64,
                &mut value,
            ))?;
            if value.is_null() {
                return Err(self.api.create_error_status(0, "No value found"));
            }
            Value {
                value: &mut *value,
                api: &self.api,
            }
        };
        // This needs a refactor! The shape should be passed here,
        // rather than when creating the `Value`.
        let element_count = value
            .get_tensor_type_and_shape()?
            .get_tensor_shape_element_count()?;

        let mut ptr: *mut _ = std::ptr::null_mut();
        let data = unsafe {
            self.api.GetTensorMutableData.unwrap()(value.value, &mut ptr);
            std::slice::from_raw_parts_mut(ptr as *mut T, element_count as usize)
        };

        let a = ArrayViewMut::from(data).into_shape(shape).unwrap();
        Ok(a)
    }
}

impl ElementType {
    pub fn to_ort_encoding(&self) -> u32 {
        match self {
            Self::F32 => ONNXTensorElementDataType_ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT,
            Self::F64 => ONNXTensorElementDataType_ONNX_TENSOR_ELEMENT_DATA_TYPE_DOUBLE,
            Self::I32 => ONNXTensorElementDataType_ONNX_TENSOR_ELEMENT_DATA_TYPE_INT32,
            Self::I64 => ONNXTensorElementDataType_ONNX_TENSOR_ELEMENT_DATA_TYPE_INT64,
            Self::String => ONNXTensorElementDataType_ONNX_TENSOR_ELEMENT_DATA_TYPE_STRING,
        }
    }
}

impl<'s> SessionOptions<'s> {
    pub fn from_ort(api_base: &mut OrtApiBase, options: &'s mut OrtSessionOptions) -> Self {
        // Version 12 is the latest one supported by the installed
        // onnxruntime. I should probably downgrade the c api file.
        let api = unsafe { api_base.GetApi.unwrap()(12) };
        Self {
            api: unsafe { &*api },
            session_options: options,
        }
    }

    pub fn create_custom_op_domain(&mut self, domain: &str) -> Result<CustomOpDomain> {
        let fun_ptr = self.api.CreateCustomOpDomain.unwrap();
        let mut domain_ptr: *mut OrtCustomOpDomain = std::ptr::null_mut();

        // Copies and leaks!
        let c_op_domain = str_to_c_char_ptr(domain);
        unsafe {
            // According to docs: "Must be freed with OrtApi::ReleaseCustomOpDomain"
            status_to_result(fun_ptr(c_op_domain, &mut domain_ptr))?;
            status_to_result(self.api.AddCustomOpDomain.unwrap()(
                self.session_options,
                domain_ptr,
            ))?;
            Ok(CustomOpDomain {
                api: self.api,
                custom_op_domain: &mut *domain_ptr,
            })
        }
    }
}

fn str_to_c_char_ptr(s: &str) -> *const c_char {
    CString::new(s).unwrap().into_raw()
}

/// Wraps a status pointer into a result.
///
///A null pointer is mapped to the `Ok(())`.
fn status_to_result(ptr: OrtStatusPtr) -> Result<()> {
    if ptr.is_null() {
        Ok(())
    } else {
        Err(ptr)
    }
}

// Leaky way to just print the error message.
fn status_to_result_dbg(ptr: OrtStatusPtr, api: &OrtApi) -> Result<()> {
    if ptr.is_null() {
        Ok(())
    } else {
        // let cstr_ptr = unsafe { api.GetErrorMessage.unwrap()(ptr) };
        // unsafe { dbg!(CString::from_raw(cstr_ptr as *mut _)) };
        Err(ptr)
    }
}

impl ExecutionProviders {
    /// Execution provider as null terminated string with static
    /// lifetime.
    pub const fn as_c_char_ptr(&self) -> &'static c_char {
        let null_term_str = match self {
            Self::Cpu => b"CPUExecutionProvider\0".as_ptr(),
        };
        unsafe { &*(null_term_str as *const _) }
    }
}