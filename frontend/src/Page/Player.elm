module Page.Player exposing (Model, Msg(..), init, update, view)

import Api exposing (Entry(..))
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (on, onClick)
import Http
import Json.Decode as D


type Model
    = Loading String
    | Loaded PlayerState
    | Failed String


type alias PlayerState =
    { path : String
    , title : String
    , uploadDate : Maybe String
    , durationSecs : Maybe Float
    , mediaError : Maybe MediaErrorCode
    }


{-| MediaError.code values from the HTML media element spec.
-}
type MediaErrorCode
    = ErrAborted
    | ErrNetwork
    | ErrDecode
    | ErrSrcNotSupported
    | ErrUnknown Int


type Msg
    = GotListing (Result Http.Error Api.DirListing)
    | GoBack
    | MediaError MediaErrorCode


init : String -> ( Model, Cmd Msg )
init path =
    let
        -- Fetch the parent directory listing to retrieve video metadata.
        parentPath =
            path
                |> String.split "/"
                |> List.reverse
                |> List.drop 1
                |> List.reverse
                |> String.join "/"
    in
    ( Loading path, Api.getBrowse parentPath GotListing )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GoBack ->
            ( model, Cmd.none )

        GotListing (Ok listing) ->
            let
                path =
                    loadingPath model

                matched =
                    listing.entries
                        |> List.filterMap
                            (\entry ->
                                case entry of
                                    Video v ->
                                        if v.path == path then
                                            Just
                                                { path = v.path
                                                , title =
                                                    Maybe.withDefault v.name
                                                        v.title
                                                , uploadDate = v.uploadDate
                                                , durationSecs = v.durationSecs
                                                , mediaError = Nothing
                                                }

                                        else
                                            Nothing

                                    _ ->
                                        Nothing
                            )
                        |> List.head
                        |> Maybe.withDefault
                            { path = path
                            , title = path
                            , uploadDate = Nothing
                            , durationSecs = Nothing
                            , mediaError = Nothing
                            }
            in
            ( Loaded matched, Cmd.none )

        GotListing (Err _) ->
            -- Metadata load failed; still show the player with minimal info.
            let
                path =
                    loadingPath model
            in
            ( Loaded
                { path = path
                , title = path
                , uploadDate = Nothing
                , durationSecs = Nothing
                , mediaError = Nothing
                }
            , Cmd.none
            )

        MediaError code ->
            case model of
                Loaded state ->
                    ( Loaded { state | mediaError = Just code }, Cmd.none )

                _ ->
                    ( model, Cmd.none )


view : Model -> Html Msg
view model =
    case model of
        Loading _ ->
            p [ style "padding" "1rem" ] [ text "Loading…" ]

        Failed err ->
            p [ style "padding" "1rem", style "color" "var(--color-error)" ]
                [ text ("Error: " ++ err) ]

        Loaded state ->
            div [ style "padding" "1rem" ]
                [ button
                    [ onClick GoBack
                    , style "margin-bottom" "1rem"
                    , style "cursor" "pointer"
                    ]
                    [ text "← Back" ]
                , case state.mediaError of
                    Just code ->
                        div
                            [ style "background" "var(--color-surface)"
                            , style "padding" "2rem"
                            , style "max-width" "960px"
                            , style "text-align" "center"
                            ]
                            [ p [ style "color" "var(--color-error)" ]
                                [ text (mediaErrorMessage code) ]
                            , a
                                [ href (Api.videoUrl state.path)
                                , attribute "download" ""
                                ]
                                [ text "Download to play in VLC or another media player" ]
                            ]

                    Nothing ->
                        video
                            [ src (Api.videoUrl state.path)
                            , controls True
                            , on "error" (D.map MediaError mediaErrorDecoder)
                            , style "width" "100%"
                            , style "max-width" "960px"
                            , style "display" "block"
                            ]
                            []
                , h2 [ style "margin-top" "0.75rem" ] [ text state.title ]
                , case state.uploadDate of
                    Just date ->
                        p [] [ text ("Uploaded: " ++ formatDate date) ]

                    Nothing ->
                        text ""
                , case state.durationSecs of
                    Just secs ->
                        p [] [ text ("Duration: " ++ formatDuration secs) ]

                    Nothing ->
                        text ""
                ]


loadingPath : Model -> String
loadingPath model =
    case model of
        Loading p ->
            p

        _ ->
            ""


{-| Format a yt-dlp upload\_date string (YYYYMMDD) as YYYY-MM-DD. -}
formatDate : String -> String
formatDate date =
    if String.length date == 8 then
        String.slice 0 4 date
            ++ "-"
            ++ String.slice 4 6 date
            ++ "-"
            ++ String.slice 6 8 date

    else
        date


formatDuration : Float -> String
formatDuration secs =
    let
        total =
            round secs

        hours =
            total // 3600

        minutes =
            (total - hours * 3600) // 60

        seconds =
            total - hours * 3600 - minutes * 60

        pad n =
            if n < 10 then
                "0" ++ String.fromInt n

            else
                String.fromInt n
    in
    if hours > 0 then
        String.fromInt hours ++ ":" ++ pad minutes ++ ":" ++ pad seconds

    else
        String.fromInt minutes ++ ":" ++ pad seconds


mediaErrorDecoder : D.Decoder MediaErrorCode
mediaErrorDecoder =
    D.at [ "target", "error", "code" ] D.int
        |> D.map
            (\code ->
                case code of
                    1 ->
                        ErrAborted

                    2 ->
                        ErrNetwork

                    3 ->
                        ErrDecode

                    4 ->
                        ErrSrcNotSupported

                    _ ->
                        ErrUnknown code
            )


mediaErrorMessage : MediaErrorCode -> String
mediaErrorMessage code =
    case code of
        ErrAborted ->
            "Playback was aborted."

        ErrNetwork ->
            "A network error prevented the video from loading."

        ErrDecode ->
            "The video could not be decoded (codec error)."

        ErrSrcNotSupported ->
            "This video format is not supported by your browser (AV1/WebM)."

        ErrUnknown n ->
            "Playback failed (error code " ++ String.fromInt n ++ ")."
