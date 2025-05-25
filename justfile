build-image:
	podman build . -t europe-west4-docker.pkg.dev/rw-dev-tf/whf/backend

push-image: build-image
	podman push europe-west4-docker.pkg.dev/rw-dev-tf/whf/backend

deploy-image: push-image
	gcloud run deploy whf-backend \
		--image=europe-west4-docker.pkg.dev/rw-dev-tf/whf/backend:latest \
		--region=europe-west4 \
		--project=rw-dev-tf
	# gcloud run services update-traffic whf-backend --to-latest --region=europe-west4
