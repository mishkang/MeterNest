function actionError(code) {
  const error = new Error(code)
  error.code = code
  return error
}

module.exports = { actionError }
